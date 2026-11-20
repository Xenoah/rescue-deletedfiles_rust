"""Compare GNU ddrescue coverage, mapped RAW bytes and files recovered from real deletions."""

import argparse
import json
import lzma
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check, run
from e2e_android_sparse import carve, originals, sha
from e2e_extended_filesystems import make_deleted
from e2e_html import make_deleted_fat
from e2e_vhdx import GOLD, recover


def invoke(binary, source, mapping, target, *args, expected=0):
    proc = subprocess.run(
        [
            str(v)
            for v in (
                binary,
                "import-ddrescue",
                "--source",
                source,
                "--map",
                mapping,
                "--destination",
                target,
                "--json",
                *args,
            )
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=dict(os.environ, PATH=""),
        timeout=180,
    )
    check(proc.returncode == expected, (proc.returncode, proc.stdout, proc.stderr))
    return json.loads(proc.stdout) if not expected else proc.stderr


def verify(binary, report, original_map, expected, finished, failed, untried):
    raw = Path(report["path"])
    detail = report["ddrescue"]
    check(
        raw.read_bytes() == expected
        and report["bytes"] == len(expected)
        and report["sha256"] == sha(expected),
        report,
    )
    check(
        (raw.parent / "ddrescue.map").read_bytes() == original_map
        and detail["map_sha256"] == sha(original_map),
        detail,
    )
    info = run(binary, "read-map", "--image", raw, "--json")
    check(Path(detail["read_map"]).samefile(str(raw) + ".rmap"), detail)
    check(
        info["copied_bytes"] == detail["finished_bytes"] == finished
        and info["failed_bytes"]
        == sum(
            detail[k]
            for k in ("nontrimmed_bytes", "nonscraped_bytes", "bad_sector_bytes")
        )
        == failed
        and info["unattempted_bytes"]
        == detail["untried_bytes"] + detail["outside_map_bytes"]
        == untried,
        (info, detail),
    )
    return raw


def portable(binary, root):
    files = originals()
    payload = b"".join(data + bytes((-len(data)) % 512) for data in files.values())
    source = root / "acquisition.raw"
    tail = b"stale contents!!" * 32
    check(len(tail) == 512, "test geometry")
    source.write_bytes(tail + payload + tail * 5)
    mapping = root / "acquisition.log"
    base = len(payload) + 512
    rows = [
        b"# producer comment \xff",
        b"0x0 + 1 # finished phase, partial media",
        f"01000 0x{len(payload):X} +".encode(),
    ]
    for n, status in enumerate(["*", "/", "-", "?", "+"]):
        rows.append(f"{base + n*512} 512 {status}".encode())
    mapping.write_bytes(b"\r\n".join(rows))
    expected = bytes(512) + payload + bytes(4 * 512) + tail
    dest = root / "mapped"
    report = invoke(binary, source, mapping, dest)
    raw = verify(
        binary,
        report,
        mapping.read_bytes(),
        expected,
        len(payload) + 512,
        3 * 512,
        2 * 512,
    )
    result = carve(binary, raw, root / "carve", files, True)
    check(report["ddrescue"]["outside_map_bytes"] == 512, report)
    for n in range(4):
        out = root / f"stale-{n}"
        run(
            binary,
            "recover-by-hash",
            "--source",
            raw,
            "--output",
            out,
            "--sha256",
            sha(tail),
            "--file-bytes",
            512,
            "--first-offset",
            base + n * 512,
            "--start",
            base + n * 512,
            "--end",
            base + (n + 1) * 512,
            "--json",
            expected=4,
        )
        check(not out.exists(), "stale bytes recovered")
    invoke(binary, source, mapping, dest, expected=2)
    check(raw.read_bytes() == expected, "overwrote mapped image")

    # Both signs of an input/output offset delta, incomplete capture, and an odd final sector.
    short = root / "short"
    short.write_bytes(tail + b"end")
    partial = root / "partial.map"
    # The partial final sector is legal only at the end of the declared logical image.
    partial.write_text("0 ?\n512 515 +\n")
    r = invoke(
        binary,
        short,
        partial,
        root / "shift-left",
        "--input-position",
        512,
        "--output-position",
        0,
        "--logical-bytes",
        1027,
    )
    verify(
        binary, r, partial.read_bytes(), bytes(512) + short.read_bytes(), 515, 0, 512
    )
    partial.write_text("0 ?\n0 512 +\n")
    right = root / "right"
    right.write_bytes(b"junk" + tail)
    r = invoke(
        binary,
        right,
        partial,
        root / "shift-right",
        "--input-position",
        0,
        "--output-position",
        4,
        "--logical-bytes",
        1024,
    )
    verify(binary, r, partial.read_bytes(), tail + bytes(512), 512, 0, 512)
    partial.write_text("0 +\n0 512 +\n512 512 -\n")
    r = invoke(binary, short, partial, root / "truncated-tail", "--logical-bytes", 1024)
    verify(binary, r, partial.read_bytes(), tail + bytes(512), 512, 512, 0)
    malformed = {
        "empty": "",
        "no-blocks": "0 + 1",
        "status": "0 X 1\n0 512 +",
        "pass": "0 + 0\n0 512 +",
        "fill": "0 F 1\n0 512 +",
        "generated": "0 G 1\n0 512 +",
        "flag": "0 + 1\n0 512 .",
        "zero": "0 + 1\n0 0 +",
        "gap": "0 + 1\n0 512 +\n1024 512 ?",
        "overlap": "0 + 1\n0 1024 +\n512 512 -",
        "overflow": "0 ?\n0xfffffffffffffe00 512 +",
        "unaligned": "0 ?\n0 7 +\n7 505 -",
        "not-comment": "0 ?\n0 512 +#bad",
        "bad-number": "0 ?\n0 0890 +",
        "double-plus": "0 ?\n++0 512 +",
    }
    for name, text in malformed.items():
        partial.write_text(text)
        out = root / ("reject-" + name)
        invoke(binary, source, partial, out, expected=4)
        check(not out.exists(), name)
    partial.write_text("0 +\n0 1024 +")
    out = root / "too-short"
    invoke(binary, short, partial, out, "--logical-bytes", 1024, expected=4)
    check(not out.exists(), "truncated copied range published")
    print(
        "GNU ddrescue map: seven exact files, all five block states, stale-data refusals, shifted captures and malformed inputs passed",
        flush=True,
    )
    return result


def command(*args):
    return subprocess.run(
        list(map(str, args)),
        check=True,
        capture_output=True,
        timeout=180,
        env=dict(os.environ, LC_ALL="C"),
    ).stdout


def native(binary, root):
    results = {}
    files = originals()
    for filesystem in ["fat32", "ext4", "windows-ntfs"]:
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            stage = Path(tmp)
            if filesystem == "fat32":
                disk = make_deleted_fat(stage, files)
            elif filesystem == "ext4":
                for name, data in files.items():
                    (stage / name).write_bytes(data)
                disk = make_deleted(stage, "ext4", files)
            else:
                manifest = json.loads((GOLD / "fixed/manifest.json").read_text())
                container = stage / "win.vhdx"
                disk = stage / "win.raw"
                with lzma.open(GOLD / "fixed/volume.vhdx.xz") as src, container.open(
                    "wb"
                ) as dst:
                    shutil.copyfileobj(src, dst)
                command(
                    "qemu-img", "convert", "-f", "vhdx", "-O", "raw", container, disk
                )
            original = disk.read_bytes()
            if filesystem == "windows-ntfs":
                check(sha(original) == manifest["guest_sha256"], "Windows guest digest")
            capture = stage / "capture.dd"
            mapping = stage / "capture.map"
            command("ddrescue", "-q", disk, capture, mapping)
            check(command("ddrescuelog", "-D", mapping) == b"", "GNU coverage")
            r = invoke(binary, capture, mapping, stage / "complete")
            raw = verify(binary, r, mapping.read_bytes(), original, len(original), 0, 0)
            proofs = (
                recover(binary, raw, stage / "restore", manifest)
                if filesystem == "windows-ntfs"
                else carve(binary, raw, stage / "restore", files, False)
            )
            results[filesystem] = dict(
                original_disk_sha256=sha(original), recovered=proofs
            )
            print(
                filesystem,
                len(proofs),
                "genuine unlinks, independently matched by bytes and SHA-256",
                flush=True,
            )
            if filesystem == "windows-ntfs":
                continue
            victim = "sample.wav"
            first = original.find(files[victim])
            check(first >= 0 and original.count(files[victim]) == 1, "unique WAV")
            bad = (first + 511) // 512 * 512
            check(bad + 512 <= first + len(files[victim]), "whole sector inside WAV")
            scenario = stage / "simulated-media.map"

            def mask(path, pos, flag):
                path.write_text(
                    f"0 + 1\n0 {pos} +\n{pos} 512 {flag}\n{pos+512} {len(original)-pos-512} +\n"
                )

            mask(scenario, bad, "-")
            domain = stage / "domain.map"
            skip = len(original) - 1024
            mask(domain, skip, "?")
            damaged = stage / "fault.dd"
            badmap = stage / "fault.map"
            # Prior contents deliberately equal the original victim sector, masking the read error in RAW alone.
            shutil.copyfile(disk, damaged)
            with damaged.open("r+b") as f:
                f.seek(skip)
                f.write(bytes([0xAC]) * 512)
            command(
                "ddrescue", "-q", "-H", scenario, "-m", domain, disk, damaged, badmap
            )
            for status, sector in [("-", bad // 512), ("?", skip // 512)]:
                check(
                    command("ddrescuelog", "-b512", "-l" + status, badmap).splitlines()
                    == [str(sector).encode()],
                    (filesystem, status),
                )
            remaining = bytearray(original)
            remaining[bad : bad + 512] = remaining[skip : skip + 512] = bytes(512)
            check(
                damaged.read_bytes()[first : first + len(files[victim])]
                == files[victim],
                "stale WAV not retained by producer",
            )
            r = invoke(binary, damaged, badmap, stage / "incomplete")
            raw = verify(
                binary,
                r,
                badmap.read_bytes(),
                remaining,
                len(original) - 1024,
                512,
                512,
            )
            out = stage / "refused.wav"
            run(
                binary,
                "recover-by-hash",
                "--source",
                raw,
                "--output",
                out,
                "--sha256",
                sha(files[victim]),
                "--file-bytes",
                len(files[victim]),
                "--first-offset",
                first,
                "--start",
                first,
                "--end",
                first + len(files[victim]),
                "--json",
                expected=4,
            )
            check(not out.exists(), "failed range supplied original-looking bytes")
            other = {n: d for n, d in files.items() if n != victim}
            results[filesystem]["with_read_error"] = dict(
                failed_sector=bad // 512,
                untried_sector=skip // 512,
                stale_wav_refused=True,
                recovered=carve(binary, raw, stage / "other", other, True),
            )
            print(
                filesystem,
                "GNU test-mode fault and excluded sector agreed with ddrescuelog; six other files exact",
                flush=True,
            )
    return results


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        results = (native if args.native else portable)(
            args.binary.resolve(), Path(tmp)
        )
    if args.evidence:
        args.evidence.write_text(json.dumps(results, indent=2) + "\n")
