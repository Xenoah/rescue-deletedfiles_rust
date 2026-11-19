"""Reassemble split regular RAW images and recover genuine deleted files across member boundaries."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import lzma
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_android_sparse import carve, originals, sha
from e2e_extended_filesystems import make_deleted
from e2e_html import make_deleted_fat
from e2e_vhdx import CORPUS, GOLD, digest, native_import, recover, verify_guest


def invoke(
    binary, first, output, *options, segments=(), expected=0, auto=False, timeout=180
):
    args = [binary, "import-image", "--source", first, "--output", output, "--json"]
    if not auto:
        args += ["--image-type", "raw"]
    args += [value for p in segments for value in ("--segment", p)]
    proc = subprocess.run(
        [*map(str, args), *map(str, options)],
        capture_output=True,
        env=dict(os.environ, PATH=""),
        text=True,
        encoding="utf-8",
        timeout=timeout,
    )
    if expected is None:
        return proc.returncode
    check(
        proc.returncode == expected, (args, proc.returncode, proc.stderr, proc.stdout)
    )
    if expected:
        return proc.stderr
    return json.loads(proc.stdout)


def verify(output, original, members, result, count=True, hashed=True):
    check(
        output.read_bytes() == original
        and digest(output) == result["sha256"] == sha(original),
        output,
    )
    detail = result["raw"]
    check(
        detail["expected_length_verified"] == count
        and detail["expected_sha256_verified"] == hashed,
        detail,
    )
    offset = 0
    check(len(members) == len(detail["segments"]), detail)
    for path, row in zip(members, detail["segments"]):
        n = path.stat().st_size
        check(
            Path(row["path"]).samefile(path)
            and (row["offset"], row["bytes"], row["sha256"])
            == (offset, n, digest(path))
            and path.read_bytes() == original[offset : offset + n],
            row,
        )
        offset += n
    check(offset == result["bytes"] == len(original), offset)
    return dict(logical_sha256=result["sha256"], bytes=offset, segments=len(members))


def split_files(root, raw, files):
    # Slice inside the actual file bytes, regardless of filesystem block alignment.
    # A whole-volume comparison also covers sections unrelated to these originals.
    cuts = []
    for name, data in files.items():
        at = raw.find(data)
        check(at >= 0, ("missing contiguous test original", name))
        cuts.append(at + max(1, len(data) // 2))
    cuts = sorted(set(cuts))
    check(len(cuts) == len(files) and any(n % 512 for n in cuts), cuts)
    paths = []
    for i, (a, b) in enumerate(zip([0, *cuts], [*cuts, len(raw)]), 1):
        part = root / f"disk.{i:03}"
        part.write_bytes(raw[a:b])
        paths.append(part)
    return paths


def negative(binary, root):
    first = root / "item.000"
    middle = root / "item.001"
    last = root / "item.002"
    out = root / "output"
    a, b, c = b"first", bytes(range(65)), b"last"
    for path, data in [(first, a), (middle, b), (last, c)]:
        path.write_bytes(data)
    good = a + b + c

    def reject(*args, **kwargs):
        code = kwargs.pop("code", 4)
        message = invoke(binary, first, out, *args, expected=code, **kwargs)
        check(not out.exists(), (args, message))

    for args, code in [
        (("--raw-sha256", sha(b"wrong")), 4),
        (("--raw-bytes", len(good) + 1), 4),
        (("--raw-sha256", "xyz"), 2),
        (("--raw-sha256", sha(good)), 2),
    ]:
        reject(*args, code=code, auto=args[-1] == sha(good))
    reject(segments=[last, middle])
    reject(segments=[middle, middle], code=2)
    middle.unlink()
    reject()
    middle.write_bytes(b)
    # A missing final part in a metadataless format requires an external expectation.
    last.unlink()
    reject("--raw-sha256", sha(good))
    last.write_bytes(c)
    rpt = invoke(binary, first, out, auto=True)
    verify(out, good, [first, middle, last], rpt, False, False)
    before = out.read_bytes()
    invoke(binary, first, out, expected=2)
    check(out.read_bytes() == before, out)
    out.unlink()
    side = Path(str(middle) + ".rmap")
    side.write_bytes(b"prior coverage")
    reject(code=2)
    check(side.read_bytes() == b"prior coverage", side)
    side.unlink()
    invoke(binary, first, side, expected=4)
    check(not side.exists(), side)
    next_segment = root / "item.003"
    invoke(binary, first, next_segment, expected=2)
    check(not next_segment.exists(), next_segment)
    four = root / "wide"
    four.mkdir()
    wide = [four / f"disk.{i:04}" for i in range(3)]
    for path, data in zip(wide, [a, b, c]):
        path.write_bytes(data)
    wide_out = four / "complete.raw"
    verify(
        wide_out, good, wide, invoke(binary, wide[0], wide_out, auto=True), False, False
    )
    empty = root / "empty.seg"
    empty.touch()
    reject(segments=[empty])
    alias = root / "hardlinked.part"
    os.link(middle, alias)
    reject(segments=[middle, alias], code=2)
    arbitrary = [root / "xaa", root / "xab", root / "xac"]
    for p, data in zip(arbitrary, [a, b, c]):
        p.write_bytes(data)
    wrong = invoke(
        binary,
        arbitrary[0],
        out,
        "--raw-sha256",
        sha(good),
        segments=arbitrary[:0:-1],
        expected=4,
    )
    check(not out.exists() and "SHA-256" in wrong, wrong)
    with ThreadPoolExecutor(max_workers=3) as pool:
        codes = list(
            pool.map(
                lambda _: invoke(
                    binary,
                    arbitrary[0],
                    out,
                    "--raw-sha256",
                    sha(good),
                    segments=arbitrary[1:],
                    expected=None,
                ),
                range(3),
            )
        )
    check(codes.count(0) == 1 and out.read_bytes() == good, codes)


def portable(binary, root):
    baseline = root / "windows.raw"
    container = root / "fixed.vhdx"
    manifest = json.loads((GOLD / "fixed/manifest.json").read_text())
    with (
        lzma.open(GOLD / "fixed/volume.vhdx.xz") as source,
        container.open("wb") as target,
    ):
        shutil.copyfileobj(source, target)
    verify_guest(baseline, native_import(binary, container, baseline, "vhdx"), manifest)
    files = {name: (CORPUS / name).read_bytes() for name in manifest["originals"]}
    raw = baseline.read_bytes()
    members = split_files(root, raw, files)
    out = root / "joined.raw"
    result = invoke(
        binary,
        members[0],
        out,
        "--raw-bytes",
        len(raw),
        "--raw-sha256",
        manifest["guest_sha256"],
    )
    detail = verify(out, raw, members, result)
    detail["originals"] = recover(binary, out, root / "scan-normal", manifest)
    check(detail["originals"] == manifest["originals"], detail)
    test = root / "negative"
    test.mkdir()
    negative(binary, test)
    print(
        "Official Windows NTFS: seven genuine deletions, eight unaligned numbered segments, complete Windows guest digest, seven recovered originals and individual segment hashes matched; invalid source sequences and hashes refused",
        flush=True,
    )
    return detail


def native(binary, root):
    files = originals()
    proof = {}
    for fs in ("fat32", "ext4"):
        stage = root / fs
        stage.mkdir()
        for name, data in files.items():
            (stage / name).write_bytes(data)
        source = (
            make_deleted_fat(stage, files)
            if fs == "fat32"
            else make_deleted(stage, fs, files)
        )
        raw = source.read_bytes()
        target = stage / "joined.raw"
        # Compare with GNU split and cat independently of Rescue and its native segment discovery.
        prefix = stage / "gnu-"
        subprocess.run(["split", "-b", "3000001", str(source), str(prefix)], check=True)
        members = sorted(stage.glob("gnu-*"))
        with (stage / "cat.raw").open("wb") as dst:
            subprocess.run(["cat", *members], stdout=dst, check=True)
        check((stage / "cat.raw").read_bytes() == raw, "GNU cat reference")
        row = invoke(
            binary,
            members[0],
            target,
            "--raw-bytes",
            len(raw),
            "--raw-sha256",
            sha(raw),
            segments=members[1:],
        )
        info = verify(target, raw, members, row)
        info["originals"] = carve(binary, target, stage / "ordinary", files, False)
        check(info["originals"] == {n: sha(v) for n, v in files.items()}, info)
        target.unlink()
        numbered = split_files(stage, raw, files)
        row = invoke(binary, numbered[0], target, auto=True)
        info["crossing_segments"] = verify(target, raw, numbered, row, False, False)[
            "segments"
        ]
        check(
            carve(binary, target, stage / "indexed", files, False, True)
            == info["originals"],
            fs,
        )
        check(source.read_bytes() == raw, "source modified")
        proof[fs] = info
        print(
            f"{fs}: seven actual deletions exact after GNU split and after seven within-file cuts; serial and indexed recovery verified",
            flush=True,
        )
    return proof


def large(binary, root):
    from e2e_vhdx_chunk import relocated_guest

    boundary = 4 << 30
    image, windows = relocated_guest(binary, root, boundary)
    size = image.stat().st_size
    cuts = [0, boundary + 257, size]
    parts = []
    for i, (a, b) in enumerate(zip(cuts, cuts[1:])):
        part = root / f"guest.{i + 1:03}"
        parts.append(part)
        subprocess.run(
            [
                "dd",
                "if=" + str(image),
                "of=" + str(part),
                "bs=4M",
                "iflag=skip_bytes,count_bytes",
                "skip=" + str(a),
                "count=" + str(b - a),
                "conv=sparse",
                "status=none",
            ],
            check=True,
        )
    expected = digest(image)
    out = root / "restored.raw"
    report = invoke(
        binary,
        parts[0],
        out,
        "--raw-bytes",
        size,
        "--raw-sha256",
        expected,
        timeout=600,
    )
    check(
        report["bytes"] == size and report["sha256"] == expected == digest(out), report
    )
    for row, part, offset in zip(report["raw"]["segments"], parts, cuts):
        check(
            row["offset"] == offset
            and row["bytes"] == part.stat().st_size
            and row["sha256"] == digest(part),
            row,
        )
    subprocess.run(["cmp", "-s", image, out], check=True)
    result = recover(binary, out, root / "recovery", windows)
    check(result == windows["originals"], result)
    print(
        f"{size} logical bytes matched across a {cuts[1]}-byte first segment; recovered seven genuine Windows NTFS deletions spanning the 4 GiB boundary",
        flush=True,
    )
    return dict(
        logical_bytes=size,
        first_segment_bytes=cuts[1],
        logical_sha256=expected,
        originals=result,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--native", action="store_true")
    modes.add_argument("--large", action="store_true")
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        suite = large if args.large else native if args.native else portable
        proof = suite(args.binary.resolve(), Path(tmp))
    if args.evidence:
        args.evidence.write_text(json.dumps(proof, indent=2) + "\n")
