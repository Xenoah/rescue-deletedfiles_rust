"""Compare explicitly selected QEMU and VirtualBox VMDK snapshots to original guest disks."""
import argparse
from contextlib import contextmanager
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

from e2e import check
from e2e_android_sparse import carve
from e2e_qcow2_backing import checked_import
from e2e_vdi_chain import changed, verify_deleted_ext4
from e2e_vhd_chain import compare_driver
from e2e_vhdx import CORPUS, digest, native_import, recover

TESTS = Path(__file__).resolve().parents[1] / "tests"


def grain_map(path):
    with path.open("rb") as source:
        header = source.read(512)
        sectors, grain = struct.unpack_from("<QQ", header, 12)
        entries = struct.unpack_from("<I", header, 44)[0]
        directory = struct.unpack_from("<Q", header, 56)[0] * 512
        result = []
        for i in range((sectors + grain - 1) // grain):
            source.seek(directory + 4 * (i // entries))
            table = struct.unpack("<I", source.read(4))[0]
            source.seek(table * 512 + 4 * (i % entries))
            result.append(struct.unpack("<I", source.read(4))[0] if table else 0)
    return result


@contextmanager
def descriptor_edit(path, old, new):
    with path.open("rb") as src:
        header = src.read(512)
        offset, sectors = struct.unpack_from("<QQ", header, 28)
        src.seek(offset * 512)
        region = src.read(512 * sectors)
    text = region.split(b"\0", 1)[0]
    check(text.count(old) == 1, (old, text))
    updated = text.replace(old, new)
    check(len(updated) < len(region), len(updated))
    with changed(path, offset * 512, updated.ljust(len(region), b"\0")):
        yield


def controls(binary, selected, specs, oracle):
    child, middle, base = selected
    out = child.parent / "refused.raw"
    rejected = []

    def reject(label, paths=selected, flags=(), code=4):
        checked_import(binary, paths, out, flags, code, kind="vmdk")
        rejected.append(label)

    reject("child_only", [child])
    reject("middle_missing", [child, base])
    reject("base_missing", [child, middle], code=2)
    reject("parents_reordered", [child, base, middle])
    reject("extra_after_base", [base, child])
    reject("reused_path", [child, middle, middle], code=2)
    reject("wrong_guest_hash", flags=["--raw-sha256", "00" * 32])
    reject("wrong_guest_length", flags=["--raw-bytes", specs[-1]["guest_bytes"] + 512])
    if oracle:
        edits = [
            ("stale_parent_modification", middle, specs[1]["modification"].encode(), b"11111111-1111-1111-1111-111111111111"),
            ("same_cid_different_parent_uuid", middle, specs[1]["uuid"].encode(), b"22222222-2222-2222-2222-222222222222"),
            ("duplicate_ancestor_uuid", middle, specs[1]["uuid"].encode(), specs[0]["uuid"].encode()),
            ("malformed_uuid", child, specs[2]["uuid"].encode(), b"not-a-uuid"),
            ("missing_parent_modification", child, b"ddb.uuid.parentmodification=", b"other="),
            ("duplicate_parent_uuid_field", child, b"ddb.uuid.parent=", b'ddb.uuid.parent="bad"\nddb.uuid.parent='),
            ("conflicting_cid_and_uuid", child, b"parentCID=ffffffff", b'parentCID=aaaaaaaa\nparentFileNameHint="child1.vmdk"'),
            ("base_has_only_parent_modification", base, b'ddb.uuid.parentmodification="00000000', b'ddb.uuid.parentmodification="10000000'),
        ]
    else:
        parent_cid = specs[1]["cid"].encode()
        edits = [
            ("mismatched_parent_cid", child, b"parentCID=" + parent_cid, b"parentCID=aaaaaaaa"),
            ("parent_cid_absent", child, b"parentCID=" + parent_cid, b"parentCID=ffffffff"),
            ("wrong_parent_basename", child, b'parentFileNameHint="child1.vmdk"', b'parentFileNameHint="child3.vmdk"'),
            ("parent_hint_traversal", child, b'parentFileNameHint="child1.vmdk"', b'parentFileNameHint="../child1.vmdk"'),
            ("parent_hint_absolute", child, b'parentFileNameHint="child1.vmdk"', b'parentFileNameHint="/child1.vmdk"'),
            ("duplicate_parent_hint", child, b'parentFileNameHint=', b'parentFileNameHint="never-open.vmdk"\nparentFileNameHint='),
            ("missing_parent_hint", child, b"parentFileNameHint=", b"unused="),
            ("repeated_content_id", child, b"\nCID=" + specs[2]["cid"].encode(), b"\nCID=" + parent_cid),
        ]
    for label, file, old, new in edits:
        with descriptor_edit(file, old, new):
            reject(label)
    # Overwriting a physical grain keeps all CIDs and UUIDs intact.
    base_map, middle_map, child_map = map(grain_map, (base, middle, child))
    physical = next(b for b, m, c in zip(base_map, middle_map, child_map, strict=True)
                    if b > 1 and m == c == 0)
    with base.open("rb") as src:
        src.seek(physical * 512)
        value = src.read(1)[0]
    with changed(base, physical * 512, bytes([value ^ 255])):
        report = checked_import(binary, selected, out, [], kind="vmdk")
        check(digest(out) == report["sha256"] != specs[-1]["guest_sha256"], report)
        out.unlink()
        reject("altered_inherited_data_with_known_hash", flags=["--raw-sha256", specs[-1]["guest_sha256"]])
    return sorted(rejected)


def verify(binary, root, qemu=None, vbox_img=None):
    results = {}
    for case in ("qemu10-ntfs", "qemu6-fat32", "qemu10-ext4", "virtualbox-ntfs"):
        oracle = case == "virtualbox-ntfs"
        fixtures = TESTS / "vmdk-vbox-parent" if oracle else TESTS / "vmdk-chain" / case
        info = json.loads((fixtures / "manifest.json").read_text(encoding="utf-8"))
        specs = info["members"]
        folder = root / case
        folder.mkdir()
        members, guests, native, deleted = [], [], [], []
        if oracle:
            for spec in specs:
                spec.update(filename=spec["file"], guest_bytes=96 << 20,
                            reference="vhd-chain/dynamic-flushed/" + Path(spec["file"]).stem + ".vhd.guest.raw.xz")
        box = None
        if oracle and vbox_img:
            sys.path.insert(0, str(TESTS / "vdi-chain"))
            from capture import VirtualBox, require
            box = VirtualBox(vbox_img, "VMDK")
        try:
            for i, spec in enumerate(specs):
                p = folder / spec["filename"]
                with lzma.open(fixtures / (p.name + ".xz")) as archive, p.open("xb") as target:
                    shutil.copyfileobj(archive, target)
                check(digest(p) == spec["sha256"], p)
                members.insert(0, p)
                raw = folder / "guest.raw"
                report = checked_import(binary, members, raw, ["--raw-sha256", spec["guest_sha256"],
                                        "--raw-bytes", spec["guest_bytes"]], kind="vmdk")
                check(report["sha256"] == digest(raw) == spec["guest_sha256"]
                      and report["bytes"] == raw.stat().st_size == spec["guest_bytes"], report)
                compare_driver(raw, TESTS / spec["reference"])
                guests.append(report["sha256"])
                if qemu and not oracle:
                    chain = json.loads(subprocess.check_output([qemu, "info", "--backing-chain", "--output=json", p.name], cwd=folder))
                    check(len(chain) == i + 1, chain)
                    subprocess.run([qemu, "compare", "-q", "-f", "raw", "-F", "vmdk", raw.name, p.name], cwd=folder, check=True)
                    native.append(digest(raw))
                if box:
                    with raw.open("rb") as source:
                        native.append(box.compare(list(reversed(members)), source))
                    check(native[-1] == spec["guest_sha256"], p)
                if qemu and case == "qemu10-ext4":
                    history = json.loads((TESTS / "vdi-chain/manifest.json").read_text())["ext4-depth8"]["deleted"]
                    deleted.append(verify_deleted_ext4(binary, raw, folder / f"tsk-{i}", history[i], info["originals"]))
                if i + 1 < len(specs):
                    raw.unlink()
            originals = {name: (CORPUS / name).read_bytes() for name in info["originals"]}
            for indexed in (False, True):
                destination = folder / ("indexed" if indexed else "ordinary")
                if case.endswith("ntfs"):
                    files = recover(binary, raw, destination, info, indexed, folder="deletions")
                else:
                    files = carve(binary, raw, destination, originals, False, indexed)
                check(files == info["originals"], case)
            raw.unlink()
            report = native_import(binary, members[0], raw, segments=members[1:])
            check(report["sha256"] == digest(raw) == guests[-1], report)
            raw.unlink()
            result = {"guest_sha256": guests, "originals": files}
            if len(members) == 3:
                result["refused"] = controls(binary, members, specs, oracle)
            if case == "qemu6-fat32":
                explicit = grain_map(members[1]).count(1)
                check(explicit == 16, explicit)
                result["explicit_zero_grains"] = explicit
            if case == "qemu10-ext4":
                # The fixture already uses eight members. Using one more must fail before writing.
                extra = folder / "extra.vmdk"
                shutil.copyfile(members[-1], extra)
                native_import(binary, members[0], raw, "vmdk", expected=4, segments=members[1:] + [extra])
            if native:
                result["native_guest_sha256"] = native
            if deleted:
                result["ext4_deletions_by_generation"] = deleted
            for path, spec in zip(reversed(members), specs, strict=True):
                check(digest(path) == spec["sha256"], path)
            results[case] = result
            print(case, len(guests), "guest disks and seven deleted originals match every byte and SHA-256", flush=True)
        finally:
            if box:
                require(box.shutdown())
        shutil.rmtree(folder)
    return results


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--qemu-img", type=Path)
    parser.add_argument("--vbox-img", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.qemu_img, args.vbox_img)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
