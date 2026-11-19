"""Recover regularly spaced forward and backwards fragments with whole-file SHA-256 evidence."""

import argparse
import json
import random
import re
import tempfile
from pathlib import Path

from e2e import check, run
from e2e_fingerprinted import samples, sha
from e2e_whole_hash import geometry, mtool, recover, verify_result
from e2e_imaging import mark_gap


def recover_stride(
    binary,
    image,
    out,
    original,
    spans,
    raw,
    unit,
    run_size,
    stride,
    detect=False,
    extra=(),
):
    if detect:
        args = ["--first-offset", spans[0][0], "--max-stride-blocks", abs(stride) + 1]
    else:
        args = ["--stride-blocks=" + str(stride)]
    report = recover(
        binary,
        image,
        out,
        original,
        "--block-size",
        unit,
        "--run-blocks",
        run_size,
        *args,
        *extra,
    )
    verify_result(report, out, original, raw, spans)
    check(
        report["matched_stride"]
        == dict(block_size=unit, run_blocks=run_size, stride_blocks=stride),
        report,
    )
    return report


def portable(binary, root):
    rng = random.Random(4107)
    files = samples()
    image = root / "disk.raw"
    out = root / "restored"
    reports = {}
    for i, (name, original) in enumerate(files.items()):
        # Exercise complete, short final runs, a grid origin of 7, and both directions.
        unit = 256 if i % 2 == 0 else 512
        count = 1 + i % 2
        step = (count + 1 + i % 3) * (-1 if i % 2 else 1)
        first = 7 + (146 if step < 0 else 3) * unit
        raw = bytearray(rng.randbytes(85 * 1024 + 7))
        spans = []
        for j, p in enumerate(range(0, len(original), count * unit)):
            piece = original[p : p + count * unit]
            q = first + j * step * unit
            raw[q : q + len(piece)] = piece
            spans.append((q, len(piece)))
        check(len(spans) >= 3, name)
        image.write_bytes(raw)
        for detect in (False, True):
            report = recover_stride(
                binary,
                image,
                out,
                original,
                spans,
                raw,
                unit,
                count,
                step,
                detect,
                ("--start", 7),
            )
            out.unlink()
        reports[name] = sha(original)
    # Three unequal gaps must never be reported as a regular layout.
    original = files["VENDOR.BIN"]
    raw = bytearray(rng.randbytes(64 << 10))
    positions = [
        1024,
        4096,
        5120,
        10240,
        11776,
        14336,
        17920,
        21504,
        25600,
        28672,
        31232,
        37888,
    ]
    for at, offset in zip(positions, range(0, len(original), 512)):
        raw[at : at + min(512, len(original) - offset)] = original[
            offset : offset + 512
        ]
    image.write_bytes(raw)
    recover(
        binary,
        image,
        out,
        original,
        "--first-offset",
        positions[0],
        "--max-stride-blocks",
        64,
        expected=4,
    )
    check(not out.exists(), "irregular spacing")

    original = files["VENDOR.BIN"]
    raw = bytearray(rng.randbytes(64 << 10))
    unit = 512
    count = 2
    step = -3
    pieces = [original[i : i + 1024] for i in range(0, len(original), 1024)]
    # First run ends at exclusive EOF; last is short and falls before every other run.
    first = len(raw) - 1024
    spans = []
    for j, piece in enumerate(pieces):
        at = first + j * step * unit
        raw[at : at + len(piece)] = piece
        spans.append((at, len(piece)))
    image.write_bytes(raw)
    report = recover_stride(
        binary, image, out, original, spans, raw, unit, count, step, True
    )
    out.unlink()
    recover(
        binary,
        image,
        out,
        original,
        "--first-offset",
        first,
        "--max-stride-blocks",
        2,
        "--run-blocks",
        2,
        expected=4,
    )
    for args, code in [
        (["--run-blocks", 2], 2),
        (["--max-stride-blocks", 5], 2),
        (["--stride-blocks=0"], 2),
        (["--stride-blocks=1", "--run-blocks", 2], 2),
        (["--stride-blocks=-2147483648"], 2),
        (["--max-stride-blocks", 257, "--first-offset", first], 2),
        (["--stride-blocks=2", "--max-work", 1], 4),
        (["--stride-blocks=3", "--max-stride-blocks", 6, "--first-offset", first], 2),
        (["--stride-blocks=-3", "--end", first + 511], 4),
    ]:
        recover(binary, image, out, original, *args, expected=code)
        check(not out.exists(), args)
    # Excluding gaps must apply to every run, including the middle of a layout.
    clean = root / "zeros.raw"
    clean.write_bytes(bytes(16 * 512))
    acquired = Path(
        run(
            binary,
            "image",
            "--source",
            clean,
            "--destination",
            root / "mapped",
            "--block-size",
            512,
            "--json",
        )["image"]
    )
    mark_gap(acquired, 2 * 512)
    recover(
        binary,
        acquired,
        out,
        bytes(3 * 512),
        "--stride-blocks=2",
        "--first-offset",
        0,
        expected=4,
    )
    check(not out.exists(), "unknown middle run")
    actual = recover(binary, acquired, out, bytes(3 * 512), "--stride-blocks=2")
    check(
        actual["reads_with_gaps"] > 0 and out.read_bytes() == bytes(3 * 512),
        "complete alternate",
    )
    print(
        "Seven exact file types in regular forward/backward runs, unknown start or detected stride; "
        "EOF, partial final run, bad geometry, budget, and unread middle run checked",
        flush=True,
    )
    return reports


def native(binary, root):
    files = samples()
    all_results = {}
    for fmt, size, flags in [
        (12, 1440 * 1024, ["-f", "1440"]),
        (16, 32 << 20, ["-c", "2"]),
        (32, 40 << 20, ["-F", "-c", "1"]),
    ]:
        folder = root / f"fat{fmt}"
        folder.mkdir()
        image = folder / "source.raw"
        with image.open("wb") as f:
            f.truncate(size)
        mtool("mformat", "-i", image, *flags, "::")
        with image.open("rb") as f:
            data_base, unit = geometry(f.read(512))
        # Allocate FAT32 root-directory space before placing adjacent data runs.
        if fmt == 32:
            empty = folder / "empty"
            empty.touch()
            names = [f"::D{i:03}.TMP" for i in range(192)]
            for name in names:
                mtool("mcopy", "-i", image, empty, name)
            mtool("mdel", "-i", image, *names)
        patterns = {}
        for i, (name, data) in enumerate(files.items()):
            (folder / name).write_bytes(data)
            clusters = (len(data) + unit - 1) // unit
            count = 2 if i % 2 and clusters >= 5 else 1
            guard = 1 + i % 3
            holes = []
            for j, k in enumerate(range(0, clusters, count)):
                hole = f"A{i:02}{j:02}.TMP"
                for tag, n in [
                    (hole, min(clusters - k, count)),
                    (f"B{i:02}{j:02}.TMP", guard),
                ]:
                    part = folder / tag
                    part.write_bytes(bytes([70 + i]) * unit * n)
                    mtool("mcopy", "-i", image, part, "::")
                holes.append(hole)
            patterns[name] = (count, count + guard, holes)
        free = next(
            line
            for line in mtool("mdir", "-i", image, "::").decode().splitlines()
            if "bytes free" in line
        )
        filler = folder / "TAIL.BIN"
        with filler.open("wb") as f:
            f.truncate(int(re.sub(r"\D", "", free)))
        mtool("mcopy", "-i", image, filler, "::")
        layouts = {}
        for name, data in files.items():
            count, step, holes = patterns[name]
            mtool("mdel", "-i", image, *["::" + h for h in holes])
            mtool("mcopy", "-i", image, folder / name, "::")
            record = mtool("mshowfat", "-i", image, "::" + name).decode()
            spans = []
            left = len(data)
            for a, b in re.findall(r"<(\d+)(?:-(\d+))?>", record):
                length = min((int(b or a) - int(a) + 1) * unit, left)
                spans.append((data_base + (int(a) - 2) * unit, length))
                left -= length
            check(
                len(spans) >= 3
                and left == 0
                and all(
                    spans[j + 1][0] - spans[j][0] == step * unit
                    for j in range(len(spans) - 1)
                ),
                (name, record),
            )
            check(mtool("mtype", "-i", image, "::" + name) == data, name)
            layouts[name] = spans
        mtool("mdel", "-i", image, *["::" + name for name in files])
        names = mtool("mdir", "-b", "-i", image, "::").decode().upper()
        check(all(n not in names for n in files), "unlink")
        raw = image.read_bytes()
        result = {}
        for name, data in files.items():
            count, step, _ = patterns[name]
            spans = layouts[name]
            # Feed a constant upper bound, never the actual distance, in the anchored trial.
            out = folder / ("restored-" + name)
            report = recover(
                binary,
                image,
                out,
                data,
                "--block-size",
                unit,
                "--first-offset",
                spans[0][0],
                "--run-blocks",
                count,
                "--max-stride-blocks",
                10,
            )
            verify_result(report, out, data, raw, spans)
            check(report["matched_stride"]["stride_blocks"] == step, name)
            out.unlink()
            recover_stride(
                binary,
                image,
                out,
                data,
                spans,
                raw,
                unit,
                count,
                step,
                False,
                ("--start", data_base % unit),
            )
            result[name] = dict(
                sha256=sha(data),
                runs=len(spans),
                run_blocks=count,
                stride_blocks=step,
                cluster_bytes=unit,
                offsets=[p for p, _ in spans],
            )
        check(sha(image.read_bytes()) == sha(raw), "disk changed")
        all_results[str(fmt)] = result
        print(
            f"FAT{fmt}, {unit}-byte clusters: seven actual unlinks, each 3 or more runs; "
            "known stride with unknown start and detected stride with known start both byte-exact",
            flush=True,
        )
    return all_results


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--native", action="store_true")
    p.add_argument("--evidence", type=Path)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        proof = (native if args.native else portable)(args.binary.resolve(), Path(temp))
    if args.evidence:
        args.evidence.write_text(json.dumps(proof, indent=2) + "\n")
