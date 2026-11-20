"""Recover six POSIX-unlinked F2FS files using FIEMAP or pre-deletion block fingerprints."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e_fingerprinted import manifest as fingerprints
from verify_f2fs_unlinks import command, sha


def verify(binary, fixtures, temp):
    meta = json.loads((fixtures / "manifest.json").read_text(encoding="utf-8"))
    files = meta["deleted_originals"]
    disk = temp / "fragmented.raw"
    with lzma.open(fixtures / "disk.raw.xz") as src, disk.open("xb") as out:
        shutil.copyfileobj(src, out)
    assert sha(disk) == meta["disk_sha256"]
    report = {"version": subprocess.check_output([binary, "--version"], text=True).strip(), "binary_sha256": sha(binary), "files": {}}
    hashes = {entry["sha256"]:name for name, entry in files.items()}
    for label, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
        result, _ = command(binary, "scan", "--mode", "carve", "--extension", "bmp,jpg,png,sqlite,wav,zip", "--source", disk,
                            "--work-dir", temp / label, *flags, incomplete=True)
        rows, _ = command(binary, "results", "--session", result["session"], "--limit", 1000)
        actual = {row["record_sha256"]:row["id"] for row in rows["candidates"] if row["supported"] and row["record_sha256"] in hashes}
        exact = []
        if actual:
            ids = [p for ident in actual.values() for p in ("--id", ident)]
            restored, _ = command(binary, "recover", "--session", result["session"], *ids, "--destination", temp / (label + "-out"))
            for row in restored["results"]:
                dest = Path(row["path"])
                original = fixtures / "files" / hashes[row["sha256"]]
                assert dest.read_bytes() == original.read_bytes() and sha(dest) == row["sha256"]
                exact.append(original.name)
        report[label] = {"candidates": len(rows["candidates"]), "exact": sorted(exact)}
    for name, entry in files.items():
        original = (fixtures / "files" / name).read_bytes()
        assert sha(fixtures / "files" / name) == entry["sha256"] and len(entry["extents"]) >= 2
        layout = temp / "layout.json"
        layout.write_text(json.dumps(dict(schema_version=1, sha256=entry["sha256"], extents=entry["extents"])), encoding="utf-8")
        output = temp / "fiemap.file"
        result, _ = command(binary, "recover-extents", "--source", disk, "--layout", layout, "--output", output)
        assert result["original_sha256_verified"] and sha(output) == entry["sha256"] and output.read_bytes() == original
        output.unlink()
        # This manifest is built only from the independent original file. No physical offsets are supplied.
        fp = temp / "fingerprints.json"
        fp.write_text(json.dumps(fingerprints(original, 4096)), encoding="utf-8")
        output = temp / "chunks.file"
        command(binary, "recover-fingerprinted", "--source", disk, "--manifest", fp, "--alignment", 4096, "--output", output)
        assert sha(output) == entry["sha256"] and output.read_bytes() == original
        output.unlink()
        report["files"][name] = {"bytes":len(original), "fiemap_ranges":len(entry["extents"]), "fiemap_sha256":entry["sha256"],
                                 "fingerprinted_sha256":entry["sha256"]}
        print(name, len(entry["extents"]), "ranges; both recovery paths exact", flush=True)
    assert sha(disk) == meta["disk_sha256"]
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, default=Path(__file__).resolve().parents[1] / "tests/f2fs-linux/fragmented")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), args.fixtures.resolve(), Path(tmp))
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
