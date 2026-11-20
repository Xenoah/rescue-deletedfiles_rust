"""Verify genuine QEMU descriptors rewritten with shorter CIDs, including embedded descriptors."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_qcow2_backing import checked_import
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


def check_rejections(binary, work, descriptor, parts, offset, cid):
    original = descriptor.read_bytes()
    capacity = 10240 if offset else len(original)
    text = original[offset:offset + capacity].partition(b"\0")[0]
    cases = {
        "unrelated_suffix": (text, b"X"),
        "extra_nul_before_suffix": (text, b"\0\n"),
        "exceeds_cid_shrink": (text, text[-(8 - len(cid)):]),
        "full_width_cid": (text.replace(("CID=" + cid + "\n").encode(), b"CID=76543210\n"), b"\n"),
        "missing_final_newline": (text.rstrip(b"\n"), b'"'),
        "non_numeric_tools_version": (text.replace(b'2147483647"', b'214748364x"'), b"\n"),
        "different_final_key": (text.replace(b"ddb.toolsVersion", b"ddb.otherVersion"), b"\n"),
        "suffix_introduces_extent": (text, b'RW 1 ZERO\n'),
        "suffix_introduces_parent": (text, b'parentCID=01234567\n'),
    }
    for label, (header, suffix) in cases.items():
        block = header + b"\0" + suffix
        check(len(block) <= capacity, label)
        descriptor.write_bytes(original[:offset] + block.ljust(capacity, b"\0") + original[offset + capacity:])
        native_import(binary, descriptor, work / "refused.raw", "vmdk", expected=4, segments=parts)
    descriptor.write_bytes(original)
    return sorted(cases)


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
            paths = [descriptor, *parts]
            report = checked_import(binary, paths, out, ["--raw-sha256", windows["guest_sha256"].upper(),
                                    "--raw-bytes", windows["guest_bytes"]], kind="vmdk")
            check(report["sha256"] == spec["guest_sha256"] == windows["guest_sha256"], name)
            verify_guest(out, report, windows)
            compare_files(raw, out)
            if independent:
                subprocess.run(["qemu-img", "compare", "-q", "-f", "vmdk", "-F", "raw", descriptor, out], check=True)
            originals = recover(binary, out, work / "ordinary", windows)
            check(recover(binary, out, work / "indexed", windows, indexed=True) == originals, name)
            check(all(digest(work / p) == m["sha256"] for p, m in spec["members"].items()), name)
            result[name] = {"guest_sha256": digest(out), "originals": originals, "retained_suffix": suffix.hex()}
            if name in ("6.2-twoGbMaxExtentFlat", "10.0-monolithicSparse"):
                refused = check_rejections(binary, work, descriptor, parts, spec["descriptor_offset"], spec["cid"])
                for label, flags, code in [
                    ("incorrect_guest_length", ["--raw-bytes", out.stat().st_size - 512], 4),
                    ("incorrect_guest_hash", ["--raw-sha256", "ff" * 32], 4),
                    ("invalid_guest_hash", ["--raw-sha256", "xyz"], 2),
                ]:
                    checked_import(binary, paths, work / "refused.raw", flags, code, "vmdk")
                    refused.append(label)
                result[name]["refused"] = sorted(refused)
                check(digest(descriptor) == spec["members"]["volume.vmdk"]["sha256"], descriptor)
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
