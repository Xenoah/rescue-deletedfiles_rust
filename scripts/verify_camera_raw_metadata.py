"""Verify metadata-based recovery on native camera originals from raw.pixls.us.

Supply a directory of originals named as in tests/camera-raw/sources.json.
Both images are newly created ordinary files in a temporary directory.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check, run

SOURCES = Path(__file__).resolve().parents[1] / "tests/camera-raw/sources.json"


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def equal(left, right):
    with left.open("rb") as a, right.open("rb") as b:
        while True:
            block = a.read(1 << 20)
            if block != b.read(1 << 20):
                return False
            if not block:
                return True


def command(*args, **kwargs):
    return subprocess.run(list(map(str, args)), capture_output=True, check=True, timeout=180, **kwargs)


def make_disk(layout, sources, folder):
    disk = folder / (layout + ".raw")
    with disk.open("xb") as file:
        file.truncate(192 << 20)
    if layout == "fat32":
        command("mkfs.fat", "-F", "32", "-s", "2", disk)
    else:
        command("mke2fs", "-q", "-t", "ext4", "-b", "4096", "-F", disk)
    readback = folder / "readback"
    for source in sources:
        if layout == "fat32":
            command("mcopy", "-i", disk, source, "::/" + source.name)
            command("mcopy", "-i", disk, "::/" + source.name, readback)
        else:
            local = folder / source.name
            local.symlink_to(source)
            command("debugfs", "-w", "-R", f"write {source.name} /{source.name}", disk, cwd=folder)
            command("debugfs", "-R", f"dump /{source.name} readback", disk, cwd=folder)
        check(equal(source, readback), source.name)
        readback.unlink()
    for source in sources:
        if layout == "fat32":
            command("mdel", "-i", disk, "::/" + source.name)
        else:
            command("debugfs", "-w", "-R", "rm /" + source.name, disk)
    deleted = command("fls", "-dFrp", "-f", layout, disk).stdout.decode().lower().splitlines()
    deleted_names = {row.split("\t")[-1] for row in deleted if "\t" in row}
    check(len(deleted_names) == len(sources), deleted_names)
    for source in sources:
        check(any(name.endswith(source.name[1:]) for name in deleted_names), source.name)
    return disk


def compare_files(sources, out):
    originals = {digest(p): p for p in sources}
    recovered = {}
    for path in out.rglob("*"):
        if not path.is_file():
            continue
        key = digest(path)
        if key in originals:
            p = originals[key]
            check(p.name not in recovered and equal(p, path), p.name)
            recovered[p.name] = key
    check(set(recovered) == {p.name for p in sources}, sorted(set(p.name for p in sources) - set(recovered)))
    return sorted(recovered)


def verify(binary, originals):
    sources = []
    manifest = json.loads(SOURCES.read_text())
    for item in manifest:
        p = originals / item["name"]
        check(p.stat().st_size == item["bytes"] and digest(p) == item["sha256"], p)
        sources.append(p)
    evidence = {}
    for layout in ("fat32", "ext4"):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            disk = make_disk(layout, sources, folder)
            before = digest(disk)
            output = folder / "filesystem-output"
            run(
                binary,
                "filesystem",
                "--source",
                disk,
                "--fs-type",
                layout,
                "--all",
                "--destination",
                output,
                "--json",
                timeout=300,
            )
            verified = compare_files(sources, output)
            shutil.rmtree(output)
            result = {"source_sha256": before, "filesystem_bytes_and_sha256": verified}
            if layout == "fat32":
                default = run(binary, "scan", "--source", disk, "--work-dir", folder / "default", "--json")
                rows = run(binary, "results", "--session", default["session"], "--json")["candidates"]
                check(len(rows) == len(sources) and not any(r["supported"] for r in rows), rows)
                scan = run(
                    binary,
                    "scan",
                    "--source",
                    disk,
                    "--estimate-fat-contiguous",
                    "--work-dir",
                    folder / "estimate",
                    "--json",
                    timeout=300,
                )
                found = run(
                    binary,
                    "recover",
                    "--session",
                    scan["session"],
                    "--all",
                    "--destination",
                    output,
                    "--json",
                    timeout=300,
                )["results"]
                check(
                    len(found) == len(sources)
                    and all(row["extent_evidence"] == "estimated_contiguous" for row in found),
                    found,
                )
                result["fat_estimate_bytes_and_sha256"] = compare_files(sources, output)
                result["default_cleared_chains_refused"] = len(rows)
            check(digest(disk) == before, disk)
            evidence[layout] = result
    version = command(binary, "--version").stdout.decode().strip()
    return {
        "version": version,
        "binary_sha256": digest(binary),
        "source_formats": len(manifest),
        "original_bytes": sum(p.stat().st_size for p in sources),
        "native_deletions": 2 * len(sources),
        "disks_unchanged": True,
        "disks": evidence,
        "file_sha256": {p.name: digest(p) for p in sources},
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--originals", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    result = verify(args.binary.resolve(), args.originals.resolve())
    report = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.report:
        args.report.write_text(report)
    print(report, end="")
