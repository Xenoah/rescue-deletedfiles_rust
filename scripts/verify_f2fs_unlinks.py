"""Compare format-based recovery with real F2FS unlinks, a retained control, and FITRIM."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile


def sha(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def command(binary, *args, incomplete=False):
    proc = subprocess.run([binary, *map(str, args), "--json"], capture_output=True, timeout=300)
    if proc.returncode not in ((0, 5) if incomplete else (0,)):
        raise RuntimeError((proc.returncode, proc.stdout, proc.stderr))
    return json.loads(proc.stdout), proc.returncode


def verify(binary, fixtures, originals, folder):
    truth = json.loads((fixtures / "manifest.json").read_text(encoding="utf-8"))
    known = {row["sha256"]: name for name, row in truth["deleted_originals"].items()}
    for checksum, name in known.items():
        assert sha(originals / name) == checksum
    control = truth["retained_control_sha256"]
    assert sha(fixtures / "retained.png") == control and control not in known
    extension = ",".join(sorted({Path(n).suffix[1:] for n in known.values()}))
    report = {"binary_sha256": sha(binary), "version": subprocess.check_output([binary, "--version"], text=True).strip(), "disks": {}}
    for label, meta in truth["disks"].items():
        with tempfile.TemporaryDirectory(dir=folder) as tmp:
            root = Path(tmp)
            source = root / "disk.raw"
            with lzma.open(fixtures / (label + ".raw.xz")) as archive, source.open("xb") as file:
                shutil.copyfileobj(archive, file)
            assert meta["bytes"] == source.stat().st_size and meta["sha256"] == sha(source)
            modes = {}
            for mode, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
                work = root / mode
                session, status = command(binary, "scan", "--mode", "carve", "--extension", extension,
                                          "--source", source, "--work-dir", work, *flags, incomplete=True)
                rows, _ = command(binary, "results", "--session", session["session"], "--limit", 1000)
                chosen = {}
                for row in rows["candidates"]:
                    checksum = row.get("record_sha256")
                    if row["supported"] and (checksum in known or checksum == control):
                        chosen.setdefault(checksum, row["id"])
                assert control in chosen, (label, mode, "retained live PNG missing")
                ids = [v for ident in chosen.values() for v in ("--id", ident)]
                restored, _ = command(binary, "recover", "--session", session["session"], *ids, "--destination", root / (mode + "-out"))
                exact = {}
                live = False
                for row in restored["results"]:
                    restored_file = Path(row["path"])
                    checksum = sha(restored_file)
                    assert checksum == row["sha256"] and checksum in chosen
                    if checksum == control:
                        assert restored_file.read_bytes() == (fixtures / "retained.png").read_bytes()
                        live = True
                    else:
                        name = known[checksum]
                        assert restored_file.read_bytes() == (originals / name).read_bytes()
                        exact[name] = checksum
                assert live and set(exact) == {known[k] for k in chosen if k != control}
                modes[mode] = {"scan_exit": status, "exact_deleted": exact, "missing_deleted": sorted(set(known.values()) - set(exact)),
                               "retained_found": live, "total_candidates": len(rows["candidates"])}
            assert modes["ordinary"]["exact_deleted"] == modes["indexed"]["exact_deleted"]
            assert sha(source) == meta["sha256"]
            report["disks"][label] = modes
            print(label, len(modes["ordinary"]["exact_deleted"]), "of", len(known), "deleted originals byte- and SHA-256-exact", flush=True)
    return report


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, default=root / "tests/f2fs-linux")
    parser.add_argument("--originals", type=Path, default=root / "tests/corpus/files")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), args.fixtures.resolve(), args.originals.resolve(), tmp)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
