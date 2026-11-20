"""Acquire only genuine Windows NTFS partitions with GNU ddrescue, then rediscover and restore them."""

import argparse
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
from e2e import check, run
from e2e_ddrescue import command, invoke, verify
from e2e_vhdx import CORPUS, GOLD, digest


def partition(data, sector):
    # The offsets are independently obtained from the original MBR or GPT.
    if sector == 512:
        record = data[446:462]
        check(record[4] == 7 and data[510:512] == b"\x55\xaa", "NTFS MBR")
        lba, count = struct.unpack_from("<II", record, 8)
    else:
        header = data[sector : 2 * sector]
        check(header[:8] == b"EFI PART", "4Kn GPT")
        table, entries, size = struct.unpack_from("<QII", header, 72)
        ntfs = []
        for i in range(entries):
            at = table * sector + i * size
            first, last = struct.unpack_from("<QQ", data, at + 32)
            if first and data[first * sector + 3 : first * sector + 11] == b"NTFS    ":
                ntfs.append((first, last - first + 1))
        ((lba, count),) = ntfs
    check(
        data[lba * sector + 3 : lba * sector + 11] == b"NTFS    ",
        "expected NTFS boot sector",
    )
    return lba * sector, count * sector


def restore(binary, stage, raw, start, length, originals, indexed):
    discovery = run(binary, "discover", "--source", raw, "--json", expected=5)
    matches = [
        r
        for r in discovery["volumes"]
        if r["region"]["offset"] == start
        and r["region"]["length"] == length
        and r["region"]["filesystem"] == "ntfs"
    ]
    check(len(matches) == 1 and not matches[0]["matches_current_table"], discovery)
    volume = matches[0]
    scan = run(
        binary,
        "scan",
        "--source",
        raw,
        "--volume-id",
        volume["id"],
        "--work-dir",
        stage / "scan",
        "--json",
        *(["--indexed"] if indexed else []),
        expected=5,
    )
    candidates = run(binary, "results", "--session", scan["session"], "--json")[
        "candidates"
    ]
    by_name = {c["name"]: c for c in candidates if c["name"] in originals}
    check(
        set(by_name) == set(originals)
        and all(
            c["supported"] and "rescue-vhdx-fixtures/" in c["original_path"]
            for c in by_name.values()
        ),
        candidates,
    )
    ids = [arg for c in by_name.values() for arg in ("--id", c["id"])]
    files = run(
        binary,
        "recover",
        "--session",
        scan["session"],
        *ids,
        "--destination",
        stage / "restored",
        "--json",
    )["results"]
    hashes = {}
    for row in files:
        path = Path(row["path"])
        check(
            row["status"] == "completed"
            and path.read_bytes() == (CORPUS / path.name).read_bytes()
            and row["sha256"] == digest(path) == originals[path.name],
            row,
        )
        hashes[path.name] = row["sha256"]
    check(hashes == originals, hashes)
    return hashes


def exercise(binary, root):
    result = {}
    for fixture, sector in [("fixed", 512), ("4kn", 4096)]:
        meta = json.loads((GOLD / fixture / "manifest.json").read_text())
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            stage = Path(tmp)
            container = stage / "source.vhdx"
            with lzma.open(GOLD / fixture / "volume.vhdx.xz") as src, container.open(
                "wb"
            ) as dst:
                shutil.copyfileobj(src, dst)
            source = stage / "windows.raw"
            with source.open("wb") as dst:
                subprocess.run(
                    ["img_cat", "-i", "vhd", container],
                    stdout=dst,
                    stderr=subprocess.PIPE,
                    check=True,
                    timeout=180,
                )
            check(digest(source) == meta["guest_sha256"], "full Windows disk hash")
            original = source.read_bytes()
            start, length = partition(original, sector)
            end = start + length
            result[fixture] = {}
            for name, out_at in [
                ("partition-only", 0),
                ("larger-output-offset", start + 3 * sector),
            ]:
                with tempfile.TemporaryDirectory(dir=stage) as tmp_case:
                    case = Path(tmp_case)
                    capture = case / "capture.dd"
                    mapping = case / "capture.map"
                    # Nonzero stale prefix must never become recorded disk data.
                    capture.write_bytes(b"X" * out_at)
                    command(
                        "ddrescue",
                        "-q",
                        "-b",
                        sector,
                        "-i",
                        start,
                        "-o",
                        out_at,
                        "-s",
                        length,
                        source,
                        capture,
                        mapping,
                    )
                    check(
                        capture.stat().st_size == out_at + length
                        and capture.read_bytes()[out_at:] == original[start:end],
                        name,
                    )
                    copied = list(
                        map(
                            int,
                            command(
                                "ddrescuelog", "-b", sector, "-l+", mapping
                            ).splitlines(),
                        )
                    )
                    check(
                        copied == list(range(start // sector, end // sector)),
                        "independent GNU map",
                    )
                    report = invoke(
                        binary,
                        capture,
                        mapping,
                        case / "mapped",
                        "--input-position",
                        start,
                        "--output-position",
                        out_at,
                        "--logical-bytes",
                        len(original),
                    )
                    expected = (
                        bytes(start) + original[start:end] + bytes(len(original) - end)
                    )
                    raw = verify(
                        binary,
                        report,
                        mapping.read_bytes(),
                        expected,
                        length,
                        0,
                        len(original) - length,
                    )
                    detail = report["ddrescue"]
                    check(
                        detail["input_position"] == start
                        and detail["output_position"] == out_at,
                        detail,
                    )
                    files = restore(
                        binary,
                        case,
                        raw,
                        start,
                        length,
                        meta["originals"],
                        bool(out_at),
                    )
                    result[fixture][name] = dict(
                        guest_sha256=meta["guest_sha256"],
                        sector_bytes=sector,
                        input_offset=start,
                        output_offset=out_at,
                        partition_bytes=length,
                        capture_sha256=digest(capture),
                        mapped_sha256=report["sha256"],
                        unavailable_bytes=len(original) - length,
                        restored=files,
                    )
                    print(
                        f"{fixture} {name}: {length} bytes, seven real NTFS unlinks matched originals",
                        flush=True,
                    )
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        proof = exercise(args.binary.resolve(), Path(tmp))
    if args.evidence:
        args.evidence.write_text(json.dumps(proof, indent=2) + "\n")
