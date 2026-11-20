"""Compare real ploop and QEMU HDS generations, flat bases and directly verified standalone images."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from e2e import check, run
from e2e_known_extents import CORPUS
from verify_camera_raw_metadata import digest, equal

GOLD = CORPUS.parents[1] / "parallels"
CARDS = GOLD.parent / "partition-extents"
MIB = 1 << 20
CHANGES = {
    "base": [(12 * MIB, 2 * 65536, 0x71)],
    "first": [(12 * MIB + 1024, 512, 0x21), (12 * MIB + 65536, 65536, 0), (14 * MIB + 512, 512, 0x44)],
    "second": [
        (12 * MIB + 3072, 512, 0xDD),
        (14 * MIB + 1024, 512, 0xE2),
        (12 * MIB + 65536 + 512, 512, 0xBF),
    ],
}


def member_paths(xml):
    root = ET.parse(xml)
    names = {n.findtext("GUID"): n.findtext("File") for n in root.findall("./StorageData/Storage/Image")}
    parents = {n.findtext("GUID"): n.findtext("ParentGUID") for n in root.findall("./Snapshots/Shot")}
    result = []
    key = root.findtext("./Snapshots/TopGUID")
    while key in names:
        result.append(xml.parent / names[key])
        key = parents[key]
    return result


def call(*args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, capture_output=True, timeout=240, **kwargs)


def import_args(source, parts=(), expected=None, auto=False):
    args = ["import-image", "--source", source]
    for member in parts:
        args.extend(["--segment", member])
    if not auto:
        args.extend(["--image-type", "parallels"])
    if expected is not None:
        args.extend(["--raw-sha256", expected])
    return args


def refuse(binary, root, label, arguments):
    output = root / "must-not-exist.raw"
    p = subprocess.run([binary, *arguments, "--output", output, "--json"], capture_output=True, timeout=120)
    check(p.returncode != 0 and not output.exists(), (label, p.stdout, p.stderr))
    return json.loads(p.stdout or p.stderr)["error"]["code"]


def decode_files(binary, raw, layouts, root):
    plan, output = root / "plan.json", root / "restored"
    hashes = {}
    for filename, layout in sorted(layouts.items()):
        plan.write_text(json.dumps(layout))
        row = run(binary, "recover-extents", "--source", raw, "--layout", plan, "--output", output, "--json")
        check(
            row["original_sha256_verified"]
            and row["file"]["sha256"] == digest(output) == layout["sha256"]
            and equal(output, CORPUS / filename),
            filename,
        )
        output.unlink()
        hashes[filename] = layout["sha256"]
    return hashes


def recover(binary, disk, parts, original, folder, layouts, expected=None, auto=False):
    raw = folder / "recovered.raw"
    checksum = hashlib.sha256(original).hexdigest()
    row = run(binary, *import_args(disk, parts, expected, auto), "--output", raw, "--json", timeout=240)
    check(
        row["bytes"] == len(original)
        and row["sha256"] == digest(raw) == checksum
        and raw.read_bytes() == original,
        (disk, row),
    )
    files = decode_files(binary, raw, layouts, folder)
    raw.unlink()
    return dict(sha256=checksum, bytes=len(original), recovered=files)


def qemu_layers(members, raw_base, reference, folder):
    args = []
    for depth, path in enumerate(members):
        pre = "backing." * depth
        driver = "raw" if raw_base and depth == len(members) - 1 else "parallels"
        # In QEMU's key/value syntax a literal comma in an argument is doubled.
        args += [
            pre + "driver=" + driver,
            pre + "file.driver=file",
            pre + "file.filename=" + str(path).replace(",", ",,"),
        ]
    output = folder / "qemu.raw"
    call("qemu-img", "convert", "--image-opts", "-O", "raw", ",".join(args), output)
    check(output.read_bytes() == reference, "QEMU logical image differs from independent original")
    output.unlink()


def rejection_cases(binary, root, source, original, descriptor, snapshot_sha256):
    controls = {}
    path = root / "broken.hds"
    clean = source.read_bytes()
    entries = struct.unpack_from("<I", clean, 32)[0]
    used = [(i, u) for i, (u,) in enumerate(struct.iter_unpack("<I", clean[64 : 64 + entries * 4])) if u]
    (ia, a), (ib, b) = used[:2]

    def hds(label, at, replacement):
        changed = bytearray(clean)
        changed[at : at + len(replacement)] = replacement
        path.write_bytes(changed)
        controls[label] = refuse(binary, root, label, import_args(path, expected=original))

    hds("signature", 0, b"Rejected HDS     ")
    for label, pos, value in [
        ("version", 16, 8),
        ("cluster_zero", 28, 0),
        ("cluster_nonpower", 28, 3),
        ("small_table", 32, 1),
        ("excessive_table", 32, 0xFFFFFFFF),
        ("zero_disk", 36, 0),
        ("in_use", 44, 0x746F6E59),
        ("data_before_table", 48, 0),
        ("conversion_in_progress", 52, 2),
        ("unknown_flags", 52, 4),
        ("unsupported_extension", 56, 1),
        ("metadata_overlap", 48, 256),
        ("alias_clusters", 64 + 4 * ib, a),
        ("past_end", 64 + 4 * ia, 0xFFFFFFFF),
    ]:
        hds(label, pos, struct.pack("<I", value))
    path.write_bytes(clean[:70])
    controls["cut_table"] = refuse(binary, root, "cut_table", import_args(path, expected=original))
    path.write_bytes(clean[:-512])
    controls["cut_data"] = refuse(binary, root, "cut_data", import_args(path, expected=original))
    hds("wrong_data_matches_length", a * 65536 + 23, bytes([clean[a * 65536 + 23] ^ 1]))
    path.write_bytes(clean)
    controls["wrong_raw_sha256"] = refuse(
        binary, root, "wrong_raw_sha256", import_args(path, expected="0" * 64)
    )
    controls["incorrect_raw_length"] = refuse(
        binary, root, "incorrect_raw_length", [*import_args(path, expected=original), "--raw-bytes", "512"]
    )
    controls["sparse_without_identity"] = refuse(binary, root, "sparse_without_identity", import_args(path))
    controls["bare_parent_hint"] = refuse(
        binary, root, "bare_parent_hint", import_args(path, [source], expected=original)
    )
    parts = member_paths(descriptor)
    xml = descriptor.read_text()
    broken = root / "broken.xml"
    top = ET.fromstring(xml).findtext("./Snapshots/TopGUID")
    leafname = parts[0].name
    for label, text in [
        ("bad_xml", xml[:-18]),
        (
            "doctype",
            xml.replace(
                "<Parallels_disk_image>",
                '<!DOCTYPE Parallels_disk_image [<!ENTITY x "test">]><Parallels_disk_image>',
                1,
            ),
        ),
        ("foreign_root", xml.replace("Parallels_disk_image", "OtherDocument")),
        ("storage_attr", xml.replace("<Storage>", "<Storage encryption='on'>", 1)),
        ("two_storages", xml.replace("</StorageData>", "<Storage/></StorageData>")),
        ("subrange", xml.replace("<Start>0</Start>", "<Start>1</Start>")),
        ("different_size", xml.replace("<End>32768</End>", "<End>32767</End>")),
        ("different_cluster", xml.replace("<Blocksize>128</Blocksize>", "<Blocksize>64</Blocksize>")),
        ("encryption", xml.replace("</Disk_Parameters>", "<Encryption>yes</Encryption></Disk_Parameters>")),
        (
            "missing_parent",
            xml.replace("{00000000-0000-0000-0000-000000000000}", "{12345678-0000-0000-0000-000000000000}"),
        ),
        ("cycle", xml.replace("{00000000-0000-0000-0000-000000000000}", top)),
        ("missing_top", xml.replace("<TopGUID>" + top, "<TopGUID>{00000000-0000-0000-0000-000000000000}")),
        (
            "duplicate_guid",
            xml.replace(
                "</Snapshots>",
                "<Shot><GUID>" + top + "</GUID><ParentGUID>" + top + "</ParentGUID></Shot></Snapshots>",
            ),
        ),
        ("unsafe_name", xml.replace(leafname, "../" + leafname)),
        ("absolute_name", xml.replace(leafname, "C:/" + leafname)),
        ("duplicate_name", xml.replace(leafname, parts[-1].name)),
        ("wrong_type", xml.replace("<Type>Compressed</Type>", "<Type>Encrypted</Type>", 1)),
    ]:
        broken.write_text(text)
        controls[label] = refuse(binary, root, label, import_args(broken, parts))
    controls["descriptor_members_missing"] = refuse(
        binary, root, "missing", import_args(descriptor, parts[:-1])
    )
    controls["spare_member"] = refuse(binary, root, "spare", import_args(descriptor, parts + [path]))
    controls["wrong_basename"] = refuse(
        binary, root, "wrong name", import_args(descriptor, [path, *parts[1:]])
    )
    renamed = root / "retained-snapshot.xml"
    shutil.copyfile(descriptor, renamed)
    controls["renamed_descriptor_missing_members"] = refuse(
        binary, root, "renamed XML", import_args(renamed, auto=True)
    )
    fork = ET.fromstring(xml)
    shots = fork.findall("./Snapshots/Shot")
    base = next(
        shot.findtext("GUID")
        for shot in shots
        if shot.findtext("ParentGUID") == "{00000000-0000-0000-0000-000000000000}"
    )
    next(shot for shot in shots if shot.findtext("GUID") == top).find("ParentGUID").text = base
    broken.write_bytes(ET.tostring(fork))
    controls["branched_snapshot_graph"] = refuse(binary, root, "fork", import_args(broken, parts))
    for label, member in [("descriptor", descriptor), ("parent", parts[-1])]:
        dest = Path(str(member) + ".rmap")
        p = subprocess.run(
            [binary, *import_args(descriptor, parts), "--output", dest, "--json"], capture_output=True
        )
        check(p.returncode != 0 and not dest.exists(), label)
        controls[label + "_reserved_read_map"] = json.loads(p.stdout or p.stderr)["error"]["code"]
    existing = root / "existing.raw"
    existing.write_bytes(b"keep original destination")
    p = subprocess.run(
        [binary, *import_args(descriptor, parts), "--output", existing, "--json"], capture_output=True
    )
    check(p.returncode != 0 and existing.read_bytes() == b"keep original destination", "collision")
    controls["existing_output_preserved"] = json.loads(p.stdout or p.stderr)["error"]["code"]
    # A valid HDS from another generation can have the same size and geometry. Only an
    # independent guest checksum can detect a different file that was given the expected basename.
    peer = root / "substitute"
    peer.mkdir()
    wrong = peer / parts[0].name
    shutil.copyfile(parts[1], wrong)
    controls["same_name_other_generation"] = refuse(
        binary, root, "wrong generation", import_args(descriptor, [wrong, *parts[1:]], snapshot_sha256)
    )
    return controls


def verify(binary, folder, native=False):
    oracle = json.loads((GOLD / "oracle.json").read_text())
    base = bytearray(lzma.decompress((CARDS / "ntfs.raw.xz").read_bytes()))
    check(hashlib.sha256(base).hexdigest() == oracle["ntfs_source_sha256"], "original NTFS capture")
    layouts = json.loads((CARDS / "layouts.json").read_text())["ntfs"]["files"]
    originals = {}
    for generation, changes in CHANGES.items():
        for offset, length, byte in changes:
            base[offset : offset + length] = bytes([byte]) * length
        originals[generation] = bytes(base)
        check(hashlib.sha256(base).hexdigest() == oracle["guest_sha256"][generation], generation)
    for name, checksum in oracle["member_sha256"].items():
        path = folder / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(lzma.decompress((GOLD / (name + ".xz")).read_bytes()))
        check(digest(path) == checksum, path)
    (folder / "flat/flat.hds").write_bytes(originals["base"])
    positive = {}
    for case, stages in [
        ("legacy", ("base", "first", "second")),
        ("extended", ("base", "first", "second")),
        ("flat", ("base", "first")),
    ]:
        d = folder / case / "DiskDescriptor.xml"
        for stage in stages:
            shutil.copyfile(GOLD / case / (stage + ".xml"), d)
            parts = member_paths(d)
            selected = parts[::-1]
            original = originals["base" if case == "flat" else stage]
            expected = hashlib.sha256(original).hexdigest() if case == "legacy" else None
            positive[case + "-" + stage] = recover(
                binary, d, selected, original, folder, layouts, expected=expected, auto=case != "legacy"
            )
            if native:
                qemu_layers(parts, case == "flat", original, folder)
        print(
            case,
            len(stages),
            "descriptor generations reproduce every guest byte and ten genuine unlinks",
            flush=True,
        )
    renamed = folder / "extended/retained.xml"
    shutil.copyfile(GOLD / "extended/second.xml", renamed)
    positive["renamed-descriptor"] = recover(
        binary, renamed, member_paths(renamed), originals["second"], folder, layouts, auto=True
    )
    complete = folder / "complete.hds"
    positive["complete-hds"] = recover(binary, complete, [], originals["base"], folder, layouts, auto=True)
    partial = folder / "extended/native.hds"
    positive["sparse-authenticated"] = recover(
        binary, partial, [], originals["base"], folder, layouts, expected=oracle["guest_sha256"]["base"]
    )
    controls = rejection_cases(
        binary,
        folder,
        partial,
        oracle["guest_sha256"]["base"],
        folder / "extended/DiskDescriptor.xml",
        oracle["guest_sha256"]["second"],
    )
    legacy = folder / "legacy/DiskDescriptor.xml"
    controls["stale_empty_flag_without_digest"] = refuse(
        binary, folder, "inconsistent empty flag", import_args(legacy, member_paths(legacy))
    )
    for name, checksum in oracle["member_sha256"].items():
        check(digest(folder / name) == checksum, name)
    print(
        len(controls),
        "refusals; all HDS and descriptor conversions verified, selected sources unchanged",
        flush=True,
    )
    result = dict(positive=positive, controls_without_output=controls, source_members_unchanged=True)
    if native:
        result["fresh_unlinks"] = fresh_unlinks(binary, folder)
    return result


def fresh_unlinks(binary, root):
    output = {}
    names = ["sample." + s for s in ("aac", "bmp", "docx", "flac", "pdf", "png", "tar", "xlsx")]
    for fs, size in [("fat32", 48), ("ext4", 16)]:
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            folder = Path(tmp)
            disk = folder / "disk.raw"
            with disk.open("xb") as f:
                f.truncate(size * MIB)
            if fs == "fat32":
                call("mkfs.fat", "-F", "32", "-s", "1", disk)
            else:
                call("mke2fs", "-q", "-F", "-t", "ext4", "-b", "1024", disk)
            copy = folder / "readback"
            for name in names:
                src = CORPUS / name
                if fs == "fat32":
                    call("mcopy", "-i", disk, src, "::/" + name)
                    call("mcopy", "-i", disk, "::/" + name, copy)
                else:
                    call("debugfs", "-w", "-R", "write " + name + " /" + name, disk, cwd=CORPUS)
                    call("debugfs", "-R", "dump /" + name + " " + str(copy), disk)
                check(equal(copy, src), name)
                copy.unlink()
            for name in names:
                if fs == "fat32":
                    call("mdel", "-i", disk, "::/" + name)
                else:
                    call("debugfs", "-w", "-R", "rm /" + name, disk)
            listing = (
                call("mdir", "-i", disk, "::/") if fs == "fat32" else call("debugfs", "-R", "ls -p /", disk)
            )
            check(all(n.encode() not in listing.stdout for n in names), listing.stdout)
            before = digest(disk)
            wrapped = folder / "card.hds"
            call(
                "qemu-img",
                "convert",
                "-f",
                "raw",
                "-O",
                "parallels",
                "-o",
                "cluster_size=4096" if fs == "fat32" else "cluster_size=1048576",
                disk,
                wrapped,
            )
            raw = folder / "recovered.raw"
            row = run(binary, *import_args(wrapped, expected=before), "--output", raw, "--json")
            check(row["sha256"] == digest(raw) == before and equal(raw, disk), fs)
            files = run(
                binary,
                "filesystem",
                "--source",
                raw,
                "--fs-type",
                fs,
                "--all",
                "--destination",
                folder / "outputs",
                "--json",
            )["results"]
            recovered = {}
            # FAT may replace the deleted first character. All corpus extensions here are distinct.
            by_extension = {Path(name).suffix: name for name in names}
            for row in files:
                saved = Path(row["path"])
                name = by_extension[saved.suffix.lower()]
                check(
                    name not in recovered
                    and row["sha256"] == digest(saved) == digest(CORPUS / name)
                    and equal(saved, CORPUS / name),
                    row,
                )
                recovered[name] = row["sha256"]
            check(set(recovered) == set(names) and digest(disk) == before, fs)
            output[fs] = recovered
            print(
                fs,
                len(names),
                "freshly deleted files recovered after independent QEMU HDS conversion",
                flush=True,
            )
    return output


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--native", action="store_true")
    p.add_argument("--output", type=Path)
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(a.binary.resolve(), Path(tmp), a.native)
    if a.output:
        a.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
