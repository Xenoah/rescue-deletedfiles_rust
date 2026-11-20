"""Recover real NTFS files from two-component QEMU VMDKs with residual descriptor suffixes."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_qcow2_backing import checked_import
from e2e_vhdx import digest, native_import, recover
from e2e_vhdx_chunk import relocated_guest
from e2e_vmdk_cid import REFERENCES, compare_files, unpack


def verify(binary, root):
    boundary = 2 << 30
    raw, windows = relocated_guest(binary, root, boundary)
    source_hash = digest(raw)
    references = REFERENCES / "boundary"
    manifest = json.loads((references / "manifest.json").read_text())
    result = {}
    for case, spec in manifest.items():
        ref = references / case
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            work = Path(tmp)
            descriptor = work / "volume.vmdk"
            if spec["kind"] == "twoGbMaxExtentFlat":
                subprocess.run(["qemu-img", "convert", "-f", "raw", "-O", "vmdk", "-o",
                                "subformat=" + spec["kind"], raw, descriptor], check=True)
            else:
                for name, metadata in spec["parts"].items():
                    packed = ref / (name + ".xz")
                    check(digest(packed) == metadata["xz_sha256"], packed)
                    unpack(packed, work / name)
            shutil.copyfile(ref / descriptor.name, descriptor)
            check(digest(descriptor) == spec["descriptor_sha256"], descriptor)
            text, nul, padding = descriptor.read_bytes().partition(b"\0")
            tail = padding.rstrip(b"\0")
            check(nul and len(tail) >= 2 and text.endswith(tail) and tail.hex() == spec["retained_suffix"], case)
            parts = [work / name for name in sorted(spec["parts"])]
            for path in parts:
                expected = spec["parts"][path.name]
                check(path.stat().st_size == expected["bytes"] and digest(path) == expected["sha256"], path)
            details = json.loads(subprocess.check_output(["qemu-img", "info", "-f", "vmdk", "--output=json", descriptor]))
            sizes = [ext["virtual-size"] for ext in details["format-specific"]["data"]["extents"]]
            check(sizes == [boundary, raw.stat().st_size - boundary] and details["virtual-size"] == spec["guest_bytes"], sizes)
            out = work / "recovered.raw"
            native_import(binary, descriptor, out, expected=4, segments=parts[:1])
            checked_import(binary, [descriptor, *reversed(parts)], out,
                           ["--raw-bytes", raw.stat().st_size, "--raw-sha256", spec["guest_sha256"]], kind="vmdk")
            check(digest(out) == source_hash == spec["guest_sha256"], out)
            compare_files(raw, out)
            subprocess.run(["qemu-img", "compare", "-q", "-f", "vmdk", "-F", "raw", descriptor, out], check=True)
            originals = recover(binary, out, work / "ordinary", windows)
            check(recover(binary, out, work / "indexed", windows, indexed=True) == originals, case)
            check(digest(descriptor) == spec["descriptor_sha256"]
                  and all(digest(p) == spec["parts"][p.name]["sha256"] for p in parts), case)
            result[case] = {"bytes": raw.stat().st_size, "guest_sha256": source_hash, "extent_bytes": sizes,
                            "retained_suffix": tail.hex(), "records_before_split": 3, "records_after_split": 4,
                            "omitted_extent_refused": True, "originals": originals}
            print(case, "2 GiB split, whole guest and seven NTFS originals match, missing extent refused", flush=True)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp))
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
