"""Verify genuine QEMU descriptors rewritten with shorter CIDs, including embedded descriptors."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_vhdx import GOLD, MIB, digest, native_import, recover, verify_guest

REFERENCES = GOLD.parent / "vmdk-cid"


def unpack(source, destination):
    with lzma.open(source, "rb") as src, destination.open("wb") as dst:
        shutil.copyfileobj(src, dst)


def compare_files(left, right):
    check(left.stat().st_size == right.stat().st_size, (left, right))
    with left.open("rb") as first, right.open("rb") as second:
        for block in iter(lambda: first.read(MIB), b""):
            check(block == second.read(len(block)), (left, right))


def verify(binary, root, independent=False):
    fixtures = json.loads((REFERENCES / "manifest.json").read_text())
    windows = json.loads((GOLD / "fixed/manifest.json").read_text())
    source = root / "windows.vhdx"
    unpack(GOLD / "fixed/volume.vhdx.xz", source)
    check(digest(source) == windows["container_sha256"], source)
    raw = root / "windows.raw"
    verify_guest(raw, native_import(binary, source, raw), windows)
    if independent:
        oracle = root / "qemu.raw"
        subprocess.run(["qemu-img", "convert", "-f", "vhdx", "-O", "raw", source, oracle], check=True)
        compare_files(oracle, raw)
        oracle.unlink()

    result = {}
    for name, spec in fixtures.items():
        with tempfile.TemporaryDirectory(dir=root) as folder:
            work = Path(folder)
            for filename, member in spec["members"].items():
                target = work / filename
                if "xz_sha256" in member:
                    packed = REFERENCES / name / (filename + ".xz")
                    check(digest(packed) == member["xz_sha256"], packed)
                    unpack(packed, target)
                else:
                    check(member["sha256"] == windows["guest_sha256"], member)
                    target.hardlink_to(raw)
                check(target.stat().st_size == member["bytes"] and digest(target) == member["sha256"], target)

            descriptor = work / "volume.vmdk"
            parts = sorted(p for p in work.glob("*.vmdk") if p != descriptor)
            with descriptor.open("rb") as src:
                src.seek(spec["descriptor_offset"])
                text, nul, padding = src.read(10240).partition(b"\0")
            suffix = padding.rstrip(b"\0")
            check(nul and suffix and suffix.hex() == spec["retained_suffix"] and text.endswith(suffix), name)
            check(("CID=" + spec["cid"]).encode() in text.splitlines(), name)
            out = work / "guest.raw"
            report = native_import(binary, descriptor, out, segments=parts)
            check(report["sha256"] == spec["guest_sha256"] == windows["guest_sha256"], name)
            verify_guest(out, report, windows)
            compare_files(raw, out)
            if independent:
                subprocess.run(["qemu-img", "compare", "-q", "-f", "vmdk", "-F", "raw", descriptor, out], check=True)
            originals = recover(binary, out, work / "ordinary", windows)
            check(recover(binary, out, work / "indexed", windows, indexed=True) == originals, name)
            check(all(digest(work / p) == m["sha256"] for p, m in spec["members"].items()), name)
            result[name] = {"guest_sha256": digest(out), "originals": originals, "retained_suffix": suffix.hex()}
            print(name, "complete guest and seven actual NTFS unlinks matched all bytes and SHA-256", flush=True)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--independent", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        result = verify(args.binary.resolve(), Path(directory), args.independent)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
