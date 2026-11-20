"""Restore partition-relative layouts with independently known original bytes and in-partition limits."""

import argparse
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_apple_partitions import make_image
from e2e_disks import disk_image
from e2e_imaging import mark_gap
from e2e_known_extents import CORPUS, manifest, restore, sha
from e2e_legacy_ext import ROOT as LEGACY, build_boundary
from verify_camera_raw_metadata import digest


def mbr_entry(kind, start, count):
    return struct.pack("<B3xB3xII", 0, kind, start, count)


def logical_disk(path, volume):
    # First partition and the EBR precede the logical volume. Its table index is 5.
    size = volume.stat().st_size
    check(size % 512 == 0, size)
    end = 48 + size // 512
    boot = bytearray(512)
    boot[510:] = b"\x55\xaa"
    boot[446:478] = mbr_entry(7, 8, 8) + mbr_entry(0x0F, 32, end - 32)
    ebr = bytearray(512)
    ebr[510:] = boot[510:]
    ebr[446:462] = mbr_entry(7, 16, size // 512)
    with path.open("xb") as dest, volume.open("rb") as source:
        dest.truncate((end + 8) * 512)
        dest.write(boot)
        dest.seek(32 * 512)
        dest.write(ebr)
        dest.seek(48 * 512)
        shutil.copyfileobj(source, dest)


def write_layout(path, layout):
    path.write_text(json.dumps(layout), encoding="utf-8")


def reject(binary, image, plan, output, layout, expected=None):
    write_layout(plan, layout)
    code = subprocess.run(
        [binary, "recover-extents", "--source", image, "--layout", plan, "--output", output, "--json"],
        capture_output=True,
    )
    check(
        code.returncode != 0 and (expected is None or code.returncode == expected) and not output.exists(),
        (layout, code),
    )


def kernel_unlinks(binary, root):
    fixture = CORPUS.parents[1] / "partition-extents"
    captures = json.loads((fixture / "layouts.json").read_text())
    results = {}
    for name, volume in captures.items():
        with tempfile.TemporaryDirectory(dir=root) as temp:
            folder = Path(temp)
            card = folder / "card.raw"
            with lzma.open(fixture / (name + ".raw.xz")) as source, card.open("xb") as target:
                shutil.copyfileobj(source, target)
            check(sha(card.read_bytes()) == volume["source_sha256"], name)
            disk = folder / "disk.raw"
            if name == "fat32":
                logical_disk(disk, card)
                index = 5
            else:
                decoy = folder / "other.raw"
                decoy.write_bytes(bytes(262144))
                disk_image(disk, [decoy, card], sector=4096, scheme="gpt")
                index = 2
            before = sha(disk.read_bytes())
            manifest_path = folder / "plan.json"
            saved = folder / "recovered"
            entries = {}
            for filename, layout in volume["files"].items():
                original = (CORPUS / filename).read_bytes()
                check(sha(original) == layout["sha256"] and len(layout["extents"]) > 1, filename)
                valid = dict(layout, schema_version=2, partition=index)
                write_layout(manifest_path, valid)
                report = restore(binary, disk, manifest_path, saved)
                check(
                    report["original_sha256_verified"]
                    and saved.read_bytes() == original
                    and report["file"]["sha256"] == sha(original),
                    filename,
                )
                entries[filename] = dict(
                    bytes=len(original), sha256=sha(original), extents=len(layout["extents"])
                )
                saved.unlink()
                reject(
                    binary,
                    disk,
                    manifest_path,
                    saved,
                    dict(valid, extents=list(reversed(layout["extents"]))),
                    4,
                )
            check(sha(disk.read_bytes()) == before, name)
            results[name] = dict(index=index, source_volume_sha256=volume["source_sha256"], files=entries)
    print(
        "20 genuine kernel unlinks across NTFS and FAT32 restored with their pre-unlink maps; 20 reversed maps refused",
        flush=True,
    )
    return results


def large_boundary(binary, root):
    disk, info, volume = build_boundary(root, json.loads((LEGACY / "manifest.json").read_text()))
    check(digest(disk) == info["sha256"], disk)
    layout = run(binary, "inspect", "--source", disk, "--json")["layout"]
    (region,) = layout["regions"]
    check(layout["scheme"] == "gpt" and region["offset"] == info["partition_offset"], region)
    files = {}
    path = root / "layout.json"
    out = root / "restored"
    for name, entry in volume["originals"].items():
        data = (CORPUS / name).read_bytes()
        offset = entry["deleted_offset"]
        plan = dict(
            schema_version=2,
            partition=region["index"],
            sha256=entry["sha256"],
            extents=[dict(offset=offset, length=entry["bytes"])],
        )
        write_layout(path, plan)
        report = restore(binary, disk, path, out)
        check(out.read_bytes() == data and sha(data) == report["file"]["sha256"] == entry["sha256"], name)
        out.unlink()
        files[name] = dict(
            bytes=entry["bytes"],
            sha256=entry["sha256"],
            volume_offset=offset,
            disk_offset=region["offset"] + offset,
        )
    wav = files["sample.wav"]
    check(wav["disk_offset"] < 1 << 32 < wav["disk_offset"] + wav["bytes"], wav)
    check(digest(disk) == info["sha256"], disk)
    print("9 kernel-deleted ext3 files restored by volume offsets across the 4 GiB boundary", flush=True)
    return dict(
        partition=region["index"],
        partition_offset=region["offset"],
        disk_sha256=info["sha256"],
        files=files,
        wav_crosses_4gib=True,
        source_unchanged=True,
    )


def exercise(binary, root):
    files = {
        ext: (CORPUS / ("sample." + ext)).read_bytes()
        for ext in ("csv", "html", "png", "pdf", "sqlite", "tar", "txt", "aac")
    }
    volume = root / "volume.raw"
    content = bytearray(4096)
    layouts = {}
    for name, data in files.items():
        cuts = [0, len(data) // 3, len(data) * 2 // 3, len(data)]
        ranges = []
        for part in (2, 1, 0):
            block = data[cuts[part] : cuts[part + 1]]
            ranges.insert(0, {"offset": len(content), "length": len(block)})
            content.extend(block)
            content.extend(b"unrelated" * 7)
        layouts[name] = manifest(data, ranges)
    content.extend(bytes((-len(content)) % 4096))
    volume.write_bytes(content)
    empty = root / "decoy.raw"
    empty.write_bytes(bytes(len(content)))
    plan = root / "layout.json"
    output = root / "restored"
    result = {}
    for label, scheme, sector, index in [
        ("dos", "mbr", 512, 2),
        ("logical", "mbr", 512, 5),
        ("gpt", "gpt", 512, 2),
        ("gpt4kn", "gpt", 4096, 2),
        ("apple", "apm", 2048, 2),
    ]:
        with tempfile.TemporaryDirectory(dir=root) as temp:
            stage = Path(temp)
            image = stage / "disk.raw"
            if scheme == "apm":
                make_image(image, sector, volume, b"Apple_UNIX_SVR2")
            elif label == "logical":
                logical_disk(image, volume)
            else:
                disk_image(image, [empty, volume], sector=sector, scheme=scheme)
            before = sha(image.read_bytes())
            table = run(binary, "inspect", "--source", image, "--json")["layout"]
            (region,) = [r for r in table["regions"] if r["index"] == index]
            check(
                table["scheme"] == scheme and region["offset"] > 0 and region["length"] == len(content), table
            )
            for name, old in layouts.items():
                layout = dict(old, schema_version=2, partition=index)
                write_layout(plan, layout)
                report = restore(binary, image, plan, output)
                check(
                    report["original_sha256_verified"]
                    and report["file"]["sha256"] == sha(output.read_bytes()) == old["sha256"]
                    and output.read_bytes() == files[name],
                    (label, name),
                )
                output.unlink()
            # A disk-absolute v1 plan remains usable against the same source.
            old = layouts["png"]
            relocated = dict(
                old, extents=[dict(e, offset=e["offset"] + region["offset"]) for e in old["extents"]]
            )
            write_layout(plan, relocated)
            restore(binary, image, plan, output)
            check(output.read_bytes() == files["png"], label)
            output.unlink()
            valid = dict(old, schema_version=2, partition=index)
            for updated in [
                {"schema_version": 1},
                {"partition": 0},
                {"partition": None},
                {"partition": 999},
                {"partition": 1},
                {"sha256": "0" * 64},
                {"partition": -1},
                {"partition": "2"},
                {"extents": [{"offset": 2**64 - 1, "length": 2}]},
            ]:
                reject(binary, image, plan, output, dict(valid, **updated))
            reject(binary, image, plan, output, dict(old, schema_version=2))
            # The following zero bytes actually exist immediately after the selected partition.
            # Accepting disk bounds instead of partition bounds would produce the expected SHA.
            reject(
                binary,
                image,
                plan,
                output,
                dict(
                    manifest(bytes(2), [{"offset": len(content) - 1, "length": 2}]),
                    schema_version=2,
                    partition=index,
                ),
                4,
            )
            sparse = dict(
                manifest(
                    b"\0" * 9 + files["png"] + bytes(11),
                    [{"zero_bytes": 9}, *old["extents"], {"zero_bytes": 11}],
                ),
                schema_version=2,
                partition=index,
            )
            write_layout(plan, sparse)
            restore(binary, image, plan, output)
            check(output.read_bytes() == bytes(9) + files["png"] + bytes(11), label)
            output.unlink()
            if label == "dos":
                acq = run(
                    binary,
                    "image",
                    "--source",
                    image,
                    "--destination",
                    stage / "acquired",
                    "--block-size",
                    "512",
                    "--json",
                )
                acquired = Path(acq["image"])
                # Neither this partition's boot sector nor the decoy boot sector is needed
                # to apply a known layout. Only the partition table and requested bytes are mandatory.
                decoy = next(other for other in table["regions"] if other["index"] != index)
                mark_gap(acquired, decoy["offset"])
                mark_gap(acquired, region["offset"])
                mark_gap(acquired, region["offset"] + 512)
                # Requested bytes are truly zero, yet the corresponding position on the disk was not acquired.
                gapped = dict(
                    manifest(bytes(128), [{"offset": 512, "length": 128}]), schema_version=2, partition=index
                )
                reject(binary, acquired, plan, output, gapped, 4)
                write_layout(plan, dict(gapped, extents=[{"offset": 1024, "length": 128}]))
                restore(binary, acquired, plan, output)
                check(output.read_bytes() == bytes(128), "adjacent acquired bytes")
                output.unlink()
            check(sha(image.read_bytes()) == before, label)
            if label == "gpt":
                # Both independent tables must lose their header CRC for this control.
                damaged = stage / "invalid-gpt.raw"
                shutil.copyfile(image, damaged)
                with damaged.open("r+b") as f:
                    for at in (sector + 16, damaged.stat().st_size - sector + 16):
                        f.seek(at)
                        value = f.read(1)[0]
                        f.seek(at)
                        f.write(bytes([value ^ 1]))
                reject(binary, damaged, plan, output, valid, 4)
            result[label] = dict(
                scheme=scheme, sector=sector, index=index, restored={n: layouts[n]["sha256"] for n in files}
            )
    print(
        "5 partition layouts, 8 file formats per layout, sparse ranges, v1 disk offsets, read-map gaps and invalid selectors checked",
        flush=True,
    )
    result["kernel_unlinks"] = kernel_unlinks(binary, root)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--large", action="store_true", help="Verify the sparse 4 GiB GPT fixture instead")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        (large_boundary if args.large else exercise)(args.binary.resolve(), Path(tmp))
