"""Recover historical NTFS, FAT32 and ext4 files from QEMU internal disk snapshots."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_android_sparse import carve
from e2e_qcow2_backing import checked_import
from e2e_vdi_chain import changed, verify_deleted_ext4
from e2e_vhd_chain import compare_driver
from e2e_vhdx import CORPUS, digest, native_import, recover

TESTS = Path(__file__).resolve().parents[1] / "tests"
GOLD = TESTS / "qcow2-snapshots"


def controls(binary, disk, snapshots, root):
    out = root / "refused.raw"
    names = []
    data = disk.read_bytes()
    u32 = lambda offset: struct.unpack_from(">I", data, offset)[0]
    u64 = lambda offset: struct.unpack_from(">Q", data, offset)[0]
    table = u64(64)
    next_entry = table + ((40 + u32(table + 36) + len(snapshots[0]["id"].encode())
                           + len(snapshots[0]["name"].encode()) + 7) & ~7)
    selected = ["--qcow2-snapshot-id", snapshots[0]["id"]]

    def reject(label, options=selected, code=4):
        checked_import(binary, [disk], out, options, code)
        names.append(label)

    reject("missing_id", ["--qcow2-snapshot-id", "absent"], 2)
    reject("missing_name", ["--qcow2-snapshot-name", "absent"], 2)
    reject("two_selectors", [*selected, "--qcow2-snapshot-name", snapshots[0]["name"]], 2)
    reject("wrong_digest", [*selected, "--raw-sha256", "00" * 32])
    reject("wrong_length", [*selected, "--raw-bytes", 512])
    first_l1, second_l1 = u64(table), u64(next_entry)
    mask = 0x00fffffffffffe00
    first_l2 = u64(first_l1) & mask
    seg = root / "unrelated.raw"
    seg.write_bytes(bytes(512))
    checked_import(binary, [disk, seg], out, selected, 2)
    names.append("external_parent_with_selector")
    for label, pos, fmt, value in [
        ("directory_unaligned", 64, "Q", table + 1),
        ("directory_refcount_alias", 64, "Q", u64(48)),
        ("directory_truncated", 64, "Q", len(data)),
        ("missing_directory", 64, "Q", 0),
        ("directory_without_count", 60, "I", 0),
        ("extra_data_length", table + 36, "I", 0xffffffff),
        ("missing_disk_size_v3", table + 36, "I", 8),
        ("l1_unaligned", table, "Q", first_l1 + 1),
        ("l1_alias_active", table, "Q", u64(40)),
        ("l1_alias_other_snapshot", table, "Q", second_l1),
        ("l1_too_small", table + 8, "I", 0),
        ("l1_reserved_flags", first_l1, "Q", u64(first_l1) | 4),
        ("l2_alias_directory", first_l1, "Q", table),
        ("l2_alias_other_snapshot_l1", first_l1, "Q", second_l1),
        ("data_alias_other_snapshot_l1", first_l2, "Q", second_l1),
        ("data_alias_directory", first_l2, "Q", table),
        ("vm_state_without_storage", table + 40, "Q", 512),
        ("zero_guest_size", table + 48, "Q", 0),
        ("l1_does_not_cover_snapshot_size", table + 48, "Q", 1 << 55),
    ]:
        with changed(disk, pos, struct.pack(">" + fmt, value)):
            reject(label)
    with changed(disk, next_entry + 40 + u32(next_entry + 36), snapshots[0]["id"].encode()):
        reject("duplicate_snapshot_id")
    with changed(disk, table + 40 + u32(table + 36), b"\xff"):
        reject("invalid_utf8_id")
    # Equal names are legal. Resolve either by ID, and report ambiguity by name.
    third = next_entry + ((40 + u32(next_entry + 36) + len(snapshots[1]["id"].encode())
                          + len(snapshots[1]["name"].encode()) + 7) & ~7)
    # Shorten both labels in-place to the same length. The padded record lengths stay fixed.
    label = b"same label!"
    first_pos = table + 40 + u32(table + 36) + 1
    third_pos = third + 40 + u32(third + 36) + 1
    for pos in (table, third):
        old = struct.unpack_from(">H", data, pos + 14)[0]
        check((40 + u32(pos + 36) + 1 + old + 7) // 8 ==
              (40 + u32(pos + 36) + 1 + len(label) + 7) // 8, label)
    with (changed(disk, table + 14, struct.pack(">H", len(label))),
          changed(disk, third + 14, struct.pack(">H", len(label))),
          changed(disk, first_pos, label), changed(disk, third_pos, label)):
        reject("ambiguous_name", ["--qcow2-snapshot-name", label.decode()], 2)
        matched = checked_import(binary, [disk], out, [*selected, "--raw-sha256", snapshots[0]["guest_sha256"]])
        check(matched["sha256"] == digest(out), matched)
        out.unlink()
    # Flip one byte of allocated guest data without changing any QCOW2 structures.
    payload = u64(first_l2) & mask
    with changed(disk, payload, bytes([data[payload] ^ 0xff])):
        matched = checked_import(binary, [disk], out, selected)
        check(digest(out) == matched["sha256"] != snapshots[0]["guest_sha256"], matched)
        out.unlink()
        reject("changed_content_with_original_sha256", [*selected, "--raw-sha256", snapshots[0]["guest_sha256"]])
    return sorted(names)


def memory_controls(binary, disk, snapshots, root):
    """Reserve machine-state clusters even when a different disk generation is selected."""
    raw = disk.read_bytes()
    word = lambda at: struct.unpack_from(">Q", raw, at)[0]
    number = lambda at: struct.unpack_from(">I", raw, at)[0]
    cluster = 1 << number(20)
    extended = number(4) == 3 and word(72) & 16
    width = 16 if extended else 8
    mask = 0x00fffffffffffe00
    directory = word(64)
    entry = directory
    for _ in snapshots[:-1]:
        i, n = struct.unpack_from(">HH", raw, entry + 12)
        entry += (40 + number(entry + 36) + i + n + 7) & ~7
    saved = snapshots[-1]
    l1 = word(entry)
    span = cluster * (cluster // width)
    ram_slot = (saved["guest_bytes"] + span - 1) // span
    ram_l2 = word(l1 + 8 * ram_slot) & mask
    ram_data = word(ram_l2) & mask
    guest_l2 = word(l1) & mask
    other_l2 = word(word(directory)) & mask
    check(ram_data > 0 and ram_l2 != guest_l2 and saved["vm_state_bytes"] > 0, entry)
    bad = [
        ("disk_mapping_aliases_saved_ram", guest_l2, ram_data, saved["id"]),
        ("earlier_disk_aliases_later_ram", other_l2, ram_data, snapshots[0]["id"]),
        ("vm_data_aliases_snapshot_directory", ram_l2, directory, saved["id"]),
        ("vm_data_aliases_another_l1", ram_l2, word(directory), saved["id"]),
        ("vm_data_beyond_source", ram_l2, (len(raw) // cluster + 1) * cluster, saved["id"]),
        ("vm_table_absent", l1 + 8 * ram_slot, 0, saved["id"]),
        ("vm_cluster_absent", ram_l2, 0, saved["id"]),
        ("compressed_vm_state", ram_l2, (1 << 62) | ram_data, saved["id"]),
        ("vm_size_exceeds_mapping", entry + 40, (1 << 64) - 1, saved["id"]),
    ]
    if extended:
        bad.append(("vm_subclusters_unallocated", ram_l2 + 8, 0, saved["id"]))
    names = []
    for label, at, value, selector in bad:
        with changed(disk, at, struct.pack(">Q", value)):
            checked_import(binary, [disk], root / "refused.raw", ["--qcow2-snapshot-id", selector], 4)
        names.append(label)
    return sorted(names)


def compare_native_snapshot(raw, reference, logical_bytes, active_bytes):
    # Some QEMU versions export a historical snapshot using the current virtual length.
    # Compare every historical byte and require the extra area, if present, to be all zero.
    checksum = hashlib.sha256()
    check(raw.stat().st_size in (logical_bytes, active_bytes), raw)
    with lzma.open(reference) as expected, raw.open("rb") as actual:
        while data := expected.read(1 << 20):
            check(actual.read(len(data)) == data, reference)
            checksum.update(data)
        check(actual.tell() == logical_bytes, reference)
        while tail := actual.read(1 << 20):
            check(not any(tail), "unexpected data after historical disk end")
    return checksum.hexdigest()


def verify(binary, root, qemu=None):
    results = {}
    history = json.loads((TESTS / "vdi-chain/manifest.json").read_text())["ext4-depth8"]["deleted"]
    for case in sorted(p for p in GOLD.iterdir() if p.is_dir() and (p / "manifest.json").is_file()):
        info = json.loads((case / "manifest.json").read_text(encoding="utf-8"))
        folder = root / case.name
        folder.mkdir()
        disk = folder / "disk.qcow2"
        raw = folder / "guest.raw"
        with lzma.open(case / "disk.qcow2.xz") as src, disk.open("xb") as dest:
            shutil.copyfileobj(src, dest)
        check(digest(disk) == info["image_sha256"], disk)
        rows = run(binary, "qcow2-snapshots", "--source", disk, "--json")["snapshots"]
        snapshots = info["snapshots"]
        check(rows == [dict(id=s["id"], name=s["name"], guest_bytes=s["guest_bytes"],
                            vm_state_bytes=s.get("vm_state_bytes", 0)) for s in snapshots], rows)
        checksums, native, deleted = [], [], []
        for index, s in enumerate(snapshots):
            for field in ("id", "name"):
                report = checked_import(binary, [disk], raw, ["--qcow2-snapshot-" + field, s[field],
                                        "--raw-sha256", s["guest_sha256"], "--raw-bytes", s["guest_bytes"]])
                check(digest(raw) == s["guest_sha256"] == report["sha256"], report)
                compare_driver(raw, TESTS / s["reference"])
                if field == "id":
                    raw.unlink()
            checksums.append(s["guest_sha256"])
            if qemu:
                other = folder / "qemu.raw"
                subprocess.run([qemu, "convert", "-l", "snapshot.id=" + s["id"], "-O", "raw", disk, other], check=True)
                native.append(compare_native_snapshot(other, TESTS / s["reference"], s["guest_bytes"], snapshots[-1]["guest_bytes"]))
                other.unlink()
                if case.name.endswith("ext4"):
                    deleted.append(verify_deleted_ext4(binary, raw, folder / f"metadata-{index}", history[min(index, len(history) - 1)], info["originals"]))
            if index + 1 < len(snapshots):
                raw.unlink()
        originals = {k: (CORPUS / k).read_bytes() for k in info["originals"]}
        for indexed in (False, True):
            dest = folder / ("indexed" if indexed else "ordinary")
            if case.name.endswith("ntfs"):
                restored = recover(binary, raw, dest, info, indexed, folder="deletions")
            else:
                restored = carve(binary, raw, dest, originals, False, indexed)
            check(restored == info["originals"], restored)
        raw.unlink()
        active = native_import(binary, disk, raw)
        with lzma.open(TESTS / snapshots[-1]["reference"]) as ref, raw.open("rb") as output:
            remaining = snapshots[-1]["guest_bytes"] - 512
            while remaining:
                expected = ref.read(min(remaining, 1 << 20))
                check(expected and output.read(len(expected)) == expected, "active disk")
                remaining -= len(expected)
            check(output.read() == bytes([0xda]) * 512, "active-only overwrite")
        check(digest(raw) == active["sha256"] == info["active_sha256"] != checksums[-1], active)
        result = dict(snapshot_sha256=checksums, active_sha256=active["sha256"], originals=restored)
        if native:
            check(native == checksums, native)
            result["qemu_snapshot_sha256"] = native
        if deleted:
            result["ext4_deletions_by_generation"] = deleted
        if snapshots[-1].get("vm_state_bytes"):
            result["vm_state_bytes"] = snapshots[-1]["vm_state_bytes"]
            result["refused"] = memory_controls(binary, disk, snapshots, folder)
        if case.name == "v3-fat32":
            result["refused"] = controls(binary, disk, snapshots, folder)
        check(digest(disk) == info["image_sha256"], disk)
        results[case.name] = result
        print(case.name, len(snapshots), "saved disks and seven deleted originals verified", flush=True)
        shutil.rmtree(folder)
    check(set(results) == {"v2-ntfs", "v3-ext4", "v3-fat32", "extended-ext4", "compressed-ntfs", "resized-ntfs", "ram-ntfs", "ram-ext4"}, results.keys())
    return results


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--qemu-img", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        result = verify(args.binary.resolve(), Path(temporary), args.qemu_img)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
