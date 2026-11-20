"""Compare forensic split captures with Windows ground truth and recover the actual NTFS unlinks."""

import argparse
import hashlib
import json
import lzma
import math
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_vhdx import GOLD, digest, recover
from e2e_split_raw import verify


def command(args, cwd=None):
    return subprocess.run(
        args,
        cwd=cwd,
        capture_output=True,
        check=True,
        timeout=180,
        encoding="utf-8",
        text=True,
        env=dict(os.environ, LC_ALL="C"),
    )


def capture(tool, naming, size, source, folder):
    prefix = "evidence"
    log = folder / "hashes.txt"
    if tool == "dcfldd":
        args = [
            tool,
            f"if={source}",
            "bs=512",
            f"split={size}",
            "hash=sha256",
            f"hashwindow={size}",
            "hashlog=hashes.txt",
        ]
        if naming != "default":
            args.append(f"splitformat={naming}")
        command([*args, f"of={prefix}"], folder)
        lines = log.read_text()
        individual = re.findall(r"(?m)^\d+ - \d+: ([0-9a-f]{64})$", lines)
        total = re.search(r"(?m)^Total \(sha256\): ([0-9a-f]{64})$", lines)
    else:
        command(
            [
                tool,
                f"if={source}",
                f"hofs={prefix}.{naming}",
                f"ofsz={size}",
                "hash=sha256",
                "log=hashes.txt",
            ],
            folder,
        )
        lines = log.read_text()
        individual = re.findall(r"\[ok\] ([0-9a-f]{64}), sectors ", lines)
        total = re.search(r"\[ok\] ([0-9a-f]{64}) \(sha256\)", lines)
    check(total is not None, "writer did not report a whole-disk digest")
    paths = list(folder.glob(prefix + ".*"))
    if naming == "MAC":
        paths.sort(key=lambda p: 1 if p.suffix == ".dmg" else int(p.suffixes[-2][1:]))
    elif naming == "aaa":
        paths.sort()
    else:
        paths.sort(key=lambda p: int(p.suffix[1:]))
    return paths, total.group(1), individual


def import_set(binary, first, target, files, length, sha256, expected=0):
    args = [
        binary,
        "import-image",
        "--image-type",
        "raw",
        "--source",
        first,
        "--output",
        target,
        "--raw-bytes",
        str(length),
        "--raw-sha256",
        sha256,
        "--json",
    ]
    for f in files:
        args.extend(["--segment", f])

    # A numbered set can have many more files than the process can open simultaneously.
    def file_limit():
        _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(
            resource.RLIMIT_NOFILE, (min(64, hard) if hard >= 0 else 64, hard)
        )

    p = subprocess.run(
        args,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=dict(os.environ, PATH=""),
        preexec_fn=file_limit,
        timeout=180,
    )
    check(p.returncode == expected, (p.returncode, p.stderr))
    if expected:
        check(not target.exists(), "failed capture published")
        return p.stderr
    return json.loads(p.stdout)


def exercise(binary, root):
    manifest = json.loads((GOLD / "fixed/manifest.json").read_text())
    with (
        lzma.open(GOLD / "fixed/volume.vhdx.xz") as src,
        (root / "win.vhdx").open("wb") as out,
    ):
        shutil.copyfileobj(src, out)
    source = root / "windows.raw"
    command(
        ["qemu-img", "convert", "-f", "vhdx", "-O", "raw", root / "win.vhdx", source]
    )
    original = source.read_bytes()
    truth = hashlib.sha256(original).hexdigest()
    check(truth == manifest["guest_sha256"], "independent Windows disk digest")
    result = {}
    for index, (tool, naming, size) in enumerate(
        [
            ("dcfldd", "default", 2 << 20),
            ("dcfldd", "%04d", 97792),
            ("dcfldd", "WIN", 3 << 20),
            ("dcfldd", "MAC", 4 << 20),
            ("dc3dd", "1111", 5 << 20),
            ("dc3dd", "aaa", 3 << 20),
        ]
    ):
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            folder = Path(tmp)
            paths, writer_sha, hashes = capture(tool, naming, size, source, folder)
            check(len(paths) == math.ceil(len(original) / size), (naming, len(paths)))
            check([digest(p) for p in paths] == hashes, "writer segment digests")
            check(writer_sha == truth, "writer's total hash")
            explicit = naming in {"MAC", "aaa"}
            extras = paths[1:] if explicit else []
            target = folder / "verified.raw"
            report = import_set(binary, paths[0], target, extras, len(original), truth)
            summary = verify(target, original, paths, report)
            summary.update(
                first=paths[0].name,
                last=paths[-1].name,
                all_writer_segment_digests_matched=True,
                explicit_order=explicit,
                originals=recover(
                    binary,
                    target,
                    folder / "recovered",
                    manifest,
                    indexed=bool(index % 2),
                ),
                restore_mode="indexed" if index % 2 else "normal",
            )
            if naming == "%04d":
                check(
                    len(paths) > 1000 and paths[1000].name.endswith(".1000"),
                    "decimal rollover",
                )
                target.unlink()
                middle = paths[999]
                temporary = middle.with_suffix(".held")
                middle.rename(temporary)
                error = import_set(
                    binary, paths[0], target, [], len(original), truth, expected=4
                )
                check("gap" in error.lower(), error)
                temporary.rename(middle)
                with middle.open("r+b") as f:
                    head = f.read(1)
                    f.seek(0)
                    f.write(bytes([head[0] ^ 1]))
                error = import_set(
                    binary, paths[0], target, [], len(original), truth, expected=4
                )
                check("sha-256" in error.lower(), error)
                summary.update(missing_999_refused=True, changed_999_refused=True)
            result[tool + "-" + naming] = summary
            print(
                tool,
                naming,
                len(paths),
                "segments, seven Windows unlinks matched",
                flush=True,
            )
    # dcfldd's input hash covers every window even if a suffix is reused on output.
    size = 97792
    with tempfile.TemporaryDirectory(dir=root) as temp:
        folder = Path(temp)
        paths, writer_sha, windows = capture("dcfldd", "default", size, source, folder)
        count = math.ceil(len(original) / size)
        check(
            len(windows) == count and writer_sha == truth, "original-size capture log"
        )
        total = sum(p.stat().st_size for p in paths)
        wrap = dict(
            expected_segments=count,
            present_segments=len(paths),
            expected_bytes=len(original),
            present_bytes=total,
            input_sha256=writer_sha,
        )
        if len(paths) == count:
            check([digest(p) for p in paths] == windows, "writer grew suffix")
            wrap["result"] = "complete"
        else:
            check(len(paths) == 1000 and 1000 < count < 2000, wrap)
            stale_tail = 0
            for n, path in enumerate(paths):
                logical = n + 1000 if n + 1000 < count else n
                piece = original[logical * size : (logical + 1) * size]
                actual = path.read_bytes()
                check(actual[: len(piece)] == piece, ("wrapped segment", n, logical))
                tail = actual[len(piece) :]
                if tail:
                    check(
                        logical == count - 1
                        and tail == original[n * size + len(piece) : (n + 1) * size],
                        ("unexpected trailing bytes", n),
                    )
                    stale_tail += len(tail)
            target = folder / "refused.raw"
            failure = import_set(
                binary, paths[0], target, [], len(original), truth, expected=4
            )
            check("length" in failure.lower(), failure)
            # Use the remaining length to independently exercise the whole-digest check.
            failure = import_set(binary, paths[0], target, [], total, truth, expected=4)
            check("sha-256" in failure.lower(), failure)
            wrap.update(
                result="reused_suffix",
                earlier_segments_replaced=count - 1000,
                stale_bytes_after_final_piece=stale_tail,
                length_rejected=True,
                digest_rejected=True,
            )
        print("dcfldd three-digit overflow", wrap, flush=True)
    return dict(
        cli_version=command([binary, "--version"]).stdout.strip(),
        cli_sha256=digest(binary),
        windows_guest_sha256=truth,
        descriptor_soft_limit=64,
        captures=result,
        dcfldd_default_overflow=wrap,
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--evidence", type=Path, required=True)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        proof = exercise(args.binary.resolve(), Path(temp))
    args.evidence.write_text(json.dumps(proof, indent=2) + "\n")
