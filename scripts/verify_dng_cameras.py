"""Verify recovery of pinned camera submissions on kernel-unlinked NTFS and ext4 images."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check, run
from verify_camera_raw_metadata import digest, equal

MANIFEST = Path(__file__).resolve().parents[1] / "tests/dng/cameras.json"


def samples(path, folder):
    # LibRaw's PGM contains raw sensor samples, without demosaicing or auto-scaling.
    with tempfile.TemporaryDirectory(dir=folder) as tmp:
        local = Path(tmp) / "sample.dng"
        shutil.copyfile(path, local)
        subprocess.run(["unprocessed_raw", "-q", local], check=True, capture_output=True, timeout=120)
        (pgm,) = Path(tmp).glob("*.pgm")
        return digest(pgm)


def scan(binary, disk, folder, indexed=False):
    flags = ["--indexed"] if indexed else []
    proc = subprocess.run(
        [
            binary,
            "scan",
            "--source",
            disk,
            "--mode",
            "carve",
            "--extension",
            "dng",
            "--work-dir",
            folder,
            "--json",
            *flags,
        ],
        capture_output=True,
        timeout=300,
    )
    check(proc.returncode in (0, 5), proc.stderr)
    session = json.loads(proc.stdout)["session"]
    rows = run(binary, "results", "--session", session, "--limit", "1000", "--json")["candidates"]
    return session, rows


def verify(binary, originals, ntfs, ext4, folder):
    manifest = json.loads(MANIFEST.read_text())
    input_files = {m["name"]: originals / m["name"] for m in manifest}
    expected = {}
    decoded = {}
    for m in manifest:
        name = m["name"]
        p = input_files[name]
        check(p.stat().st_size == m["bytes"] and digest(p) == m["sha256"], name)
        data = p.read_bytes()
        if m["referenced_end"] is not None:
            key = hashlib.sha256(data[: m["referenced_end"]]).hexdigest()
            check(key == m["referenced_sha256"], name)
            expected[key] = m
        decoded[name] = samples(p, folder)
    before = {kind: digest(disk) for kind, disk in [("ntfs", ntfs), ("ext4", ext4)]}
    meta = run(binary, "scan", "--source", ntfs, "--work-dir", folder / "ntfs-meta", "--json")
    check(meta["candidates"] == 0, meta)
    check(
        not run(binary, "filesystem", "--source", ntfs, "--fs-type", "ntfs", "--json")["candidates"],
        "NTFS deleted metadata",
    )
    carved = {}
    for label in ("ordinary", "indexed"):
        session, rows = scan(binary, ntfs, folder / label, indexed=(label == "indexed"))
        valid = [r for r in rows if r["supported"]]
        check({r["record_sha256"] for r in valid} == expected.keys() and len(valid) == len(expected), valid)
        saved = {}
        for row in valid:
            item = expected[row["record_sha256"]]
            name = item["name"]
            p = input_files[name]
            (rec,) = run(
                binary,
                "recover",
                "--session",
                session,
                "--id",
                row["id"],
                "--destination",
                folder / (label + "-out"),
                "--json",
            )["results"]
            dest = Path(rec["path"])
            check(
                dest.read_bytes() == p.read_bytes()[: item["referenced_end"]]
                and digest(dest) == item["referenced_sha256"],
                name,
            )
            check(samples(dest, folder) == decoded[name], name)
            saved[name] = dict(
                offset=row["record"],
                bytes=dest.stat().st_size,
                sha256=digest(dest),
                complete_original=equal(p, dest),
                sensor_pgm_sha256=decoded[name],
            )
            dest.unlink()
        carved[label] = saved
    check(carved["ordinary"] == carved["indexed"], "scan modes")
    check(
        not run(binary, "filesystem", "--source", ext4, "--fs-type", "ext4", "--json")["candidates"],
        "ext4 current metadata",
    )
    journal = run(binary, "ext4-journal", "--source", ext4, "--json")["candidates"]
    check(len(journal) == len(manifest), journal)
    ext4_saved = {}
    for row in journal:
        names = {x["name"] for x in row["historical_names"]}
        check(len(names) == 1, names)
        name = names.pop()
        p = input_files[name]
        dest = folder / "journal-output.dng"
        run(
            binary,
            "ext4-journal",
            "--source",
            ext4,
            "--inode",
            str(row["inode"]),
            "--sha256",
            digest(p),
            "--output",
            dest,
            "--json",
        )
        check(equal(p, dest) and digest(dest) == digest(p) and samples(dest, folder) == decoded[name], name)
        ext4_saved[name] = dict(
            inode=row["inode"], bytes=dest.stat().st_size, sha256=digest(dest), pgm_sha256=decoded[name]
        )
        dest.unlink()
    check(before == {"ntfs": digest(ntfs), "ext4": digest(ext4)}, "disk changed")
    return dict(
        disks_unchanged=True,
        disk_sha256=before,
        ntfs_orphans_with_named_metadata=0,
        ntfs_carving=carved,
        ext4_current_candidates=0,
        ext4_journal=ext4_saved,
        fully_carved=sum(r["complete_original"] for r in carved["ordinary"].values()),
        external_decoder="LibRaw unprocessed_raw PGM with no scaling",
    )


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "originals", "ntfs", "ext4", "output"):
        ap.add_argument("--" + arg, type=Path, required=True)
    args = ap.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), args.originals.resolve(), args.ntfs, args.ext4, Path(tmp))
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(
        len(result["ext4_journal"]),
        "ext4 journal originals and",
        result["fully_carved"],
        "complete NTFS3 carved originals",
    )
