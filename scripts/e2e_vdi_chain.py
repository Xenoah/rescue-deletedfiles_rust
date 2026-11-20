"""Flatten native VirtualBox snapshots and recover files actually deleted from their guest filesystems."""

import argparse
from contextlib import contextmanager
import json
import lzma
from pathlib import Path
import shutil
import struct
import sys
import tempfile

from e2e import check, run
from e2e_android_sparse import carve
from e2e_qcow2_backing import checked_import
from e2e_vhd_chain import compare_driver
from e2e_vhdx import CORPUS, digest, native_import, recover

REF = Path(__file__).resolve().parents[1] / "tests/vdi-chain"


@contextmanager
def changed(path, offset, data):
    with path.open("r+b") as f:
        f.seek(offset)
        original = f.read(len(data))
        check(len(original) == len(data), path)
        f.seek(offset)
        f.write(data)
    try:
        yield
    finally:
        with path.open("r+b") as f:
            f.seek(offset)
            f.write(original)


def controls(binary, root, members, final):
    base, middle, child = members
    chain = [child, middle, base]
    output = root / "refused.raw"
    names = []

    def reject(label, selected, args=(), code=4):
        checked_import(binary, selected, output, args, code, "vdi")
        names.append(label)

    reject("missing_all_parents", [child])
    reject("missing_base", [child, middle], code=2)
    reject("wrong_parent_order", [child, base, middle])
    reject("additional_parent", [base, middle])
    reject("repeated_path", [child, middle, middle], code=2)
    reject("wrong_expected_length", chain, ["--raw-bytes", final["guest_bytes"] + 512])
    reject("wrong_expected_hash", chain, ["--raw-sha256", "00" * 32])
    reject("malformed_hash", chain, ["--raw-sha256", "xyz"], 2)
    bad = root / "alias.vdi"
    shutil.copyfile(middle, bad)
    reject("copied_image_id_cycle", [child, middle, bad])
    bad.unlink()

    header = child.read_bytes()[:512]
    table = struct.unpack_from("<I", header, 340)[0]
    used = struct.unpack_from("<I", header, 388)[0]
    le = lambda n: struct.pack("<I", n)
    for label, file, pos, data in [
        ("missing_own_uuid", child, 392, bytes(16)),
        ("missing_modification_uuid", child, 408, bytes(16)),
        ("missing_parent_uuid", child, 424, bytes(16)),
        ("missing_expected_parent_modification", child, 440, bytes(16)),
        ("wrong_parent_uuid", middle, 392, b"a" * 16),
        ("stale_parent_modification", middle, 408, b"b" * 16),
        ("same_id_wrong_ancestor", base, 408, b"c" * 16),
        ("base_claims_parent", base, 424, b"d" * 16),
        ("unsupported_undo_type", child, 76, le(3)),
        ("unsupported_flags", child, 80, le(1)),
        ("different_parent_sector_size", base, 360, le(4096)),
        (
            "geometry_disagrees",
            child,
            368,
            struct.pack("<Q", final["guest_bytes"] - 512),
        ),
        ("map_points_inside_header", child, 340, le(32)),
        ("data_overlaps_table", child, 344, le(table)),
        ("out_of_range_slot", child, table, le(used)),
        ("missing_allocated_slot", child, 388, le(0)),
        ("duplicate_slot", child, table, le(0) + le(0)),
    ]:
        with changed(file, pos, data):
            reject(label, chain)
    # Same image IDs and lengths cannot detect a sector whose valid data was changed in place.
    # The last base block in this FAT fixture is non-zero, and the middle snapshot inherits it.
    bh = base.read_bytes()[:512]
    map_at, data_at = struct.unpack_from("<II", bh, 340)
    bs, extra, count = struct.unpack_from("<III", bh, 376)
    with base.open("rb") as f:
        f.seek(map_at + (count - 1) * 4)
        slot = struct.unpack("<I", f.read(4))[0]
    check(slot < 0xFFFFFFFE, slot)
    at = data_at + slot * (bs + extra) + extra
    with changed(base, at, b"X"):
        middle_spec = json.loads((REF / "manifest.json").read_text())["fat32-zero"][
            "members"
        ][1]
        report = checked_import(binary, [middle, base], output, [], kind="vdi")
        check(report["sha256"] == digest(output) != middle_spec["guest_sha256"], report)
        output.unlink()
        reject(
            "changed_inherited_bytes",
            [middle, base],
            ["--raw-sha256", middle_spec["guest_sha256"]],
        )
    return sorted(names)


def verify_deleted_ext4(binary, raw, folder, deleted, originals):
    if not deleted:
        # The root contains live files at this generation, before any unlink.
        listing = run(
            binary, "filesystem", "--source", raw, "--fs-type", "ext4", "--json"
        )
        check(not listing["candidates"], listing)
        return {}
    restored = run(
        binary,
        "filesystem",
        "--source",
        raw,
        "--fs-type",
        "ext4",
        "--all",
        "--destination",
        folder,
        "--json",
    )
    result = {}
    for row in restored["results"]:
        name = row["original_path"]
        check(
            name in deleted and row["id"] == str(deleted[name]) and "error" not in row,
            row,
        )
        path = Path(row["path"])
        check(
            path.read_bytes() == (CORPUS / name).read_bytes()
            and row["sha256"] == digest(path) == originals[name],
            row,
        )
        result[name] = row["sha256"]
    check(result.keys() == deleted.keys(), result)
    return result


def verify(binary, root, vbox_img=None):
    box = None
    if vbox_img:
        sys.path.insert(0, str(REF))
        from capture import VirtualBox

        box = VirtualBox(vbox_img)

    manifest = json.loads((REF / "manifest.json").read_text(encoding="utf-8"))
    result = {}
    for case, fixture in manifest.items():
        folder = root / case
        folder.mkdir()
        selected = []
        guests = []
        native = []
        deleted = []
        for i, spec in enumerate(fixture["members"]):
            image = folder / spec["filename"]
            with lzma.open(REF / case / (image.name + ".xz")) as src, image.open(
                "xb"
            ) as out:
                shutil.copyfileobj(src, out)
            check(digest(image) == spec["sha256"], image)
            selected.insert(0, image)
            raw = folder / "guest.raw"
            report = checked_import(
                binary,
                selected,
                raw,
                [
                    "--raw-sha256",
                    spec["guest_sha256"],
                    "--raw-bytes",
                    spec["guest_bytes"],
                ],
                kind="vdi",
            )
            check(
                raw.stat().st_size == report["bytes"] == spec["guest_bytes"]
                and digest(raw) == report["sha256"] == spec["guest_sha256"],
                image,
            )
            if case == "ext4-depth8":
                reference = REF / case / f"ext4-{i}.raw.xz"
            elif case == "fat32-zero":
                reference = REF / case / f"fat-{i}.raw.xz"
            else:
                reference = (
                    REF.parent / "vhd-chain" / case / (image.stem + ".vhd.guest.raw.xz")
                )
            compare_driver(raw, reference)
            if box:
                with raw.open("rb") as expected:
                    native.append(box.compare(list(reversed(selected)), expected))
                check(native[-1] == spec["guest_sha256"], image)
                if case == "ext4-depth8":
                    deleted.append(
                        verify_deleted_ext4(
                            binary,
                            raw,
                            folder / f"tsk-{i}",
                            fixture["deleted"][i],
                            fixture["originals"],
                        )
                    )
            guests.append(report["sha256"])
            if i + 1 < len(fixture["members"]):
                raw.unlink()

        paths = {k: (CORPUS / k).read_bytes() for k in fixture["originals"]}
        for indexed in (False, True):
            work = folder / ("indexed" if indexed else "ordinary")
            if case in ("fat32-zero", "ext4-depth8"):
                copies = carve(binary, raw, work, paths, False, indexed)
            else:
                copies = recover(
                    binary, raw, work, fixture, indexed, folder="deletions"
                )
            check(copies == fixture["originals"], case)
        raw.unlink()
        report = native_import(binary, selected[0], raw, segments=selected[1:])
        check(digest(raw) == report["sha256"] == guests[-1], case)
        raw.unlink()
        if case == "fat32-zero":
            result["refusals"] = controls(
                binary, folder, list(reversed(selected)), fixture["members"][-1]
            )
        for image, spec in zip(reversed(selected), fixture["members"], strict=True):
            check(digest(image) == spec["sha256"], image)
        result[case] = {"guest_sha256": guests, "originals": copies}
        if box:
            result[case]["virtualbox_sha256"] = native
            if deleted:
                result[case]["ext4_deleted_by_generation"] = deleted
        print(
            case,
            len(guests),
            "native VDI generations and 7 real deletions match original bytes and SHA-256",
            flush=True,
        )
        shutil.rmtree(folder)
    if box:
        from capture import require

        require(box.shutdown())
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--output", type=Path)
    p.add_argument(
        "--vbox-img",
        type=Path,
        help="Independently read every guest through VirtualBox and restore ext4 deletions through The Sleuth Kit",
    )
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as work:
        result = verify(a.binary.resolve(), Path(work), a.vbox_img)
    if a.output:
        a.output.write_text(
            json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8"
        )
