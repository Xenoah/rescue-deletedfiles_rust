"""Recover Linux ext2/ext3 unlinks and verify that identical live controls are excluded."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zipfile

from e2e import check, run
from e2e_disks import disk_image
from e2e_hfs_allocation import restore, scan
from e2e_imaging import mark_gap

ROOT = Path(__file__).resolve().parents[1] / "tests/legacy-ext"
EXTENSIONS = "bmp,gif,jpg,pdf,png,sqlite,wav,webp,zip"


def u32(data, offset):
    return struct.unpack_from("<I", data, offset)[0]


def independent(image, ref, kind):
    subprocess.run(["e2fsck", "-fn", image], check=True, capture_output=True, timeout=90)
    listing = subprocess.check_output(["fls", "-rp", "-f", kind, image], text=True)
    files = {}
    for line in listing.splitlines():
        record, _, name = line.partition("\t")
        if name.startswith("saved/sample.") and "* " not in record:
            files[name[6:]] = record.split()[-1].rstrip(":")
    check(set(files) == ref["originals"].keys(), listing)
    block = ref["block_bytes"]
    for name, inode in files.items():
        original = (ROOT.parent / "corpus/files" / name).read_bytes()
        live = subprocess.check_output(["icat", "-f", kind, image, inode])
        check(live == original and hashlib.sha256(live).hexdigest() == ref["originals"][name]["sha256"], name)
        for state, key in [("Not Allocated", "deleted_offset"), ("Allocated", "live_offset")]:
            pos = ref["originals"][name][key]
            for number in range(pos // block, (pos + len(original) - 1) // block + 1):
                output = subprocess.check_output(["blkstat", "-f", kind, image, str(number)], text=True)
                check(state in output.splitlines(), (name, number, output))
    return sorted(files)


def controls(binary, root, source, ref, include_journal):
    data = source.read_bytes()
    bs = ref["block_bytes"]
    first = u32(data, 1044)
    per_group = u32(data, 1056)
    descriptors = (first + 1) * bs
    bitmap = u32(data, descriptors) * bs
    inode_map = u32(data, descriptors + 4) * bs
    table = u32(data, descriptors + 8) * bs
    inode_bytes = struct.unpack_from("<H", data, 1112)[0]
    png = ref["originals"]["sample.png"]
    expected = {png[key] for key in ("live_offset", "deleted_offset")}
    def assign(pos, value):
        return lambda b: b.__setitem__(slice(pos, pos + len(value)), value)
    def flip(pos):
        return lambda b: b.__setitem__(pos, b[pos] ^ 1)
    def swap(data, block1, block2):
        for block in (block1, block2):
            group, bit = divmod(block - first, per_group)
            pos = u32(data, descriptors + group * 32) * bs + bit // 8
            data[pos] ^= 1 << (bit % 8)
    cases = {
        "unclean": assign(1082, b"\x02\x00"), "requires-replay": assign(1120, b"\x06"),
        "orphan-list": assign(1256, struct.pack("<I", 17)), "foreign-creator": assign(1096, b"\x01"),
        "invalid-fragment-size": assign(1052, struct.pack("<I", 3)), "filesystem-too-long": assign(1028, bytes([255]) * 4),
        "bad-free-block-count": flip(1036), "bad-free-inode-count": flip(1040),
        "overlapping-maps": assign(descriptors + 4, data[descriptors:descriptors+4]),
        "relocated-inode-table": assign(descriptors + 8, data[descriptors:descriptors+4]),
        "group-count": flip(descriptors + 12), "wrong-declared-directories": flip(descriptors + 16),
        "uninitialized-map": assign(descriptors + 18, b"\x02\x00"),
        "inode-bitmap-count": flip(inode_map + 8), "bitmap-padding": assign(bitmap + bs - 1, b"\x7f"),
        "invalid-root-type": assign(table + inode_bytes, bytes(2)),
        "invalid-root-pointer": assign(table + inode_bytes + 40, bytes([255]) * 4),
        "wrong-root-sector-count": flip(table + inode_bytes + 28),
    }
    # Swap a used and free block in the SAME group, preserving both free counts.
    live = png["live_offset"] // bs
    group, index = divmod(live - first, per_group)
    group_start = first + group * per_group
    group_map = u32(data, descriptors + group * 32) * bs
    free = next(k for k in range(min(per_group, u32(data,1028) - group_start))
                if not data[group_map + k//8] & (1 << (k%8)))
    check(data[group_map + index//8] & (1 << (index%8)), "live control was free")
    cases["equal-count-swap-of-live-and-free-blocks"] = lambda b: swap(b, live, group_start + free)
    if include_journal:
        journal = table + (u32(data, 1248) - 1) * inode_bytes
        header = u32(data, journal + 40) * bs
        check(data[header:header+4] == bytes.fromhex("c03b3998"), "journal header")
        cases.update({"journal-needs-replay": assign(header + 28, bytes.fromhex("00000005")),
                      "journal-wrong-uuid": flip(header + 48), "journal-size-mismatch": flip(header + 19),
                      "journal-error": flip(header + 35), "journal-external-device": flip(1252)})
    def unknown(image, label, expected_exit=0):
        summary, rows = scan(binary, image, root / (label + "-scan"), extensions="png", expected=expected_exit)
        check({r["record"] for r in rows} == expected and all(r["allocation_status"] == "unknown" for r in rows), (label, rows))
        check(any(w.startswith("ALLOCATION_FALLBACK:") for w in summary["warnings"]), summary)
    changed = root / "damaged.raw"
    for name, edit in cases.items():
        value = bytearray(data)
        edit(value)
        check(value != data, name)
        changed.write_bytes(value)
        unknown(changed, name)
    acquisition = run(binary, "image", "--source", source, "--destination", root / "map", "--block-size", "4096", "--json")
    image = Path(acquisition["image"])
    mark_gap(image, bitmap)
    unknown(image, "unread-bitmap", expected_exit=5)
    return sorted([*cases, "unread-bitmap"])


def indirect_controls(binary, folder, volume, ref):
    data = volume.read_bytes()
    bs = ref["block_bytes"]
    first = u32(data, 1044)
    groups = (u32(data, 1028) - first + u32(data,1056) - 1) // u32(data,1056)
    ipg = u32(data,1064)
    isize = struct.unpack_from("<H",data,1112)[0]
    matches = []
    for group in range(groups):
        desc = (first+1)*bs + group*32
        bitmap, table = u32(data,desc+4)*bs, u32(data,desc+8)*bs
        for local in range(ipg):
            pos = table + isize*local
            if data[bitmap+local//8] & (1 << (local%8)) and u32(data,pos+4) > 70000*bs and u32(data,pos+96):
                if struct.unpack_from("<H",data,pos+26)[0] == 2:
                    matches.append(pos)
    inode, = matches
    triple = u32(data,inode+96)
    double = u32(data,triple*bs)
    single = next(v[0] for v in struct.iter_unpack("<I",data[double*bs:(double+1)*bs]) if v[0])
    last = next(i for i,(v,) in enumerate(struct.iter_unpack("<I",data[single*bs:(single+1)*bs])) if v)
    cases = {"cyclic-triple-indirect": (triple*bs, triple), "bad-double-indirect": (triple*bs, 0xffffffff),
             "unaccounted-deep-data": (single*bs+last*4,0), "duplicate-indirect": (inode+92, double)}
    modified = folder / "bad-tree.raw"
    expected = {ref["originals"]["sample.png"][key] for key in ("deleted_offset","live_offset")}
    for name,(pos,value) in cases.items():
        invalid = bytearray(data)
        struct.pack_into("<I",invalid,pos,value)
        modified.write_bytes(invalid)
        summary, rows = scan(binary,modified,folder/name,extensions="png")
        check({r["record"] for r in rows} == expected and all(r["allocation_status"] == "unknown" for r in rows), name)
        check(any(w.startswith("ALLOCATION_FALLBACK:") for w in summary["warnings"]), name)
    return sorted(cases)


def stale_recovery(binary, root, volume, ref):
    # Reassign the second block of the still-live WAV to the byte-identical block
    # of the deleted copy. Preserve inode accounting, all content, and all free counts.
    data = bytearray(volume.read_bytes())
    bs = ref["block_bytes"]
    wav = ref["originals"]["sample.wav"]
    live, deleted = [wav[key] // bs for key in ("live_offset", "deleted_offset")]
    check(wav["bytes"] > 2 * bs, wav)
    first, groupsize, inodes = (u32(data, pos) for pos in (1044, 1056, 1064))
    groups = (u32(data, 1028) - first + groupsize - 1) // groupsize
    descriptors = (first + 1) * bs
    isize = struct.unpack_from("<H", data, 1112)[0]
    matches = []
    for g in range(groups):
        desc = descriptors + 32 * g
        table = u32(data, desc + 8) * bs
        bitmap = u32(data, desc + 4) * bs
        for number in range(inodes):
            inode = table + number * isize
            if data[bitmap + number//8] & (1 << (number%8)) and u32(data, inode + 40) == live and u32(data, inode + 4) == wav["bytes"]:
                matches.append(inode)
    inode, = matches
    check(u32(data, inode + 44) == live + 1, inode)
    wrapper = root / "reallocated.raw"
    ((start, _),) = disk_image(wrapper, [volume], sector=4096)
    shift = start * 4096
    prior, (row,) = scan(binary, wrapper, root / "prior-allocation", extensions="wav")
    check(row["record"] == shift + wav["deleted_offset"], row)
    baseline = wrapper.stat()
    struct.pack_into("<I", data, inode + 44, deleted + 1)
    for block, free_delta in [(live + 1, 1), (deleted + 1, -1)]:
        group, bit = divmod(block - first, groupsize)
        desc = descriptors + group * 32
        bitmap = u32(data, desc) * bs
        check(bool(data[bitmap + bit//8] & (1 << (bit%8))) == (free_delta > 0), block)
        data[bitmap + bit//8] ^= 1 << (bit%8)
        free = struct.unpack_from("<H", data, desc + 12)[0]
        struct.pack_into("<H", data, desc + 12, free + free_delta)
    with wrapper.open("r+b") as stream:
        stream.seek(shift)
        stream.write(data)
    os.utime(wrapper, ns=(baseline.st_atime_ns, baseline.st_mtime_ns))
    after, rows = scan(binary, wrapper, root / "after-allocation", extensions="wav")
    check(not rows and not any(w.startswith("ALLOCATION_FALLBACK:") for w in after["warnings"]), after)
    states = [json.loads(Path(scan_result["session"]).read_text(encoding="utf-8"))["fingerprint"] for scan_result in (prior, after)]
    check(states[0] == states[1], states)
    _, all_rows = scan(binary, wrapper, root / "after-all", scope="all", extensions="wav")
    check({r["record"] for r in all_rows} == {wav["deleted_offset"] + shift, wav["live_offset"] + shift}, all_rows)
    result = run(binary, "recover", "--session", prior["session"], "--all", "--destination", root / "stale-out", "--json", expected=5)
    check(result["results"][0]["path"] is None and "SOURCE_CHANGED" in json.dumps(result), result)
    return dict(second_block_reallocated=True, original_source_fingerprint_preserved=True, stale_recovery="SOURCE_CHANGED")


def report_allocation(binary, root, volume, ref):
    result = {}
    for scope, state in [("unallocated", "unallocated"), ("all", "unknown")]:
        session, candidates = scan(binary, volume, root / ("report-" + scope), scope=scope, extensions=EXTENSIONS)
        expected = {r["id"]: r["record_sha256"] for r in candidates}
        check(len(expected) == len(ref["originals"]) * (2 if scope == "all" else 1), scope)
        for output in ("directory", "archive"):
            if output == "directory":
                rows = run(binary, "recover", "--session", session["session"], "--all",
                           "--destination", root / ("files-" + scope), "--json")["results"]
                digests = {r["candidate_id"]: hashlib.sha256(Path(r["path"]).read_bytes()).hexdigest() for r in rows}
            else:
                saved = run(binary, "recover", "--session", session["session"], "--all",
                            "--archive", root / (scope + ".zip"), "--json")["result"]
                with zipfile.ZipFile(saved["archive"]) as archive:
                    rows = json.loads(archive.read("rescue-report.json"))["results"]
                    digests = {r["candidate_id"]: hashlib.sha256(archive.read(r["archive_entry"])).hexdigest() for r in rows}
            check(digests == expected and all(r["allocation_status"] == state for r in rows), (scope, output, rows))
            result[scope + "_" + output] = dict(allocation_status=state, files=len(rows))
    return result


def build_boundary(root, manifest):
    # The two GPT copies were written by GNU Parted. Rebuild the same sparse
    # device without requiring 4 GiB of physical storage on the test machine.
    folder = ROOT / "gpt-4g"
    info = json.loads((folder / "manifest.json").read_text())
    ref = manifest[info["volume"]]
    contents = lzma.decompress((ROOT / (info["volume"] + ".raw.xz")).read_bytes())
    check(hashlib.sha256(contents).hexdigest() == info["volume_sha256"] == ref["disk_sha256"], info)
    head, tail = [lzma.decompress((folder / (name + ".raw.xz")).read_bytes()) for name in ("head", "tail")]
    disk = root / "ext3-gpt-4g.raw"
    start, size = info["partition_offset"], info["bytes"]
    with disk.open("xb") as stream:
        stream.truncate(size)
        stream.write(head)
        stream.seek(start)
        stream.write(contents)
        stream.seek(size - len(tail))
        stream.write(tail)
    return disk, info, ref


def boundary(binary, root, manifest):
    disk, info, ref = build_boundary(root, manifest)
    start = info["partition_offset"]
    def digest():
        with disk.open("rb") as stream:
            return hashlib.file_digest(stream, "sha256").hexdigest()
    check(digest() == info["sha256"], disk)
    listing = subprocess.check_output(["mmls", disk], text=True)
    sector = start // info["sector_size"]
    check(str(sector) in listing and "ext3-deleted" in listing, listing)
    files = ref["originals"]
    truth = {start + f["deleted_offset"]: name for name, f in files.items()}
    limit = 1 << 32
    check(min(truth) < limit < max(truth), truth)
    wav = files["sample.wav"]
    check(start + wav["deleted_offset"] < limit < start + wav["deleted_offset"] + wav["bytes"], wav)
    for name, f in files.items():
        for key, status in [("deleted_offset","Not Allocated"),("live_offset","Allocated")]:
            fst = f[key] // ref["block_bytes"]
            last = (f[key]+f["bytes"]-1) // ref["block_bytes"]
            for block in {fst,last}:
                report = subprocess.check_output(["blkstat", "-o", str(sector), "-f", "ext3", disk, str(block)], text=True)
                check(status in report.splitlines(), (name, block, report))
    for label, indexed in [("ordinary", False), ("indexed", True)]:
        job = root / ("boundary-" + label)
        session, rows = scan(binary, disk, job, indexed=indexed, extensions=EXTENSIONS, bounds=(start, start + ref["bytes"]))
        check({r["record"] for r in rows} == truth.keys() and all(r["supported"] and r["allocation_status"] == "unallocated" for r in rows), rows)
        by_id = {r["id"]: truth[r["record"]] for r in rows}
        recovered = run(binary, "recover", "--session", session["session"], "--all", "--destination", job / "out", "--json", timeout=180)["results"]
        check(len(recovered) == len(files), recovered)
        for row in recovered:
            name = by_id[row["candidate_id"]]
            restored = Path(row["path"]).read_bytes()
            check(restored == (ROOT.parent / "corpus/files" / name).read_bytes() and
                  hashlib.sha256(restored).hexdigest() == row["sha256"] == files[name]["sha256"], name)
    check(digest() == info["sha256"], disk)
    return dict(disk_bytes=info["bytes"], disk_sha256=info["sha256"], partition_offset=start, crossing_file="sample.wav",
                restored={name: dict(offset=pos, sha256=files[name]["sha256"]) for pos,name in truth.items()})


def verify(binary, root, native=False):
    manifest = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))
    result = {}
    for name, ref in manifest.items():
        folder = root / name
        folder.mkdir()
        volume = folder / "volume.raw"
        raw = lzma.decompress((ROOT / (name + ".raw.xz")).read_bytes())
        check(len(raw) == ref["bytes"] and hashlib.sha256(raw).hexdigest() == ref["disk_sha256"], name)
        volume.write_bytes(raw)
        source = volume
        shift = 0
        if name in ("ext2-2048", "ext3-2048"):
            source = folder / "partitions.raw"
            sector = 4096 if name == "ext3-2048" else 512
            ((start, _),) = disk_image(source, [volume], sector=sector)
            shift = start * sector
        files = ref["originals"]
        _, rows = scan(binary, source, folder / "all", scope="all", extensions=EXTENSIONS)
        all_offsets = {f[key] + shift for f in files.values() for key in ("live_offset", "deleted_offset")}
        check({r["record"] for r in rows} == all_offsets and len(rows) == 2 * len(files), (name, rows))
        exact = restore(binary, source, files, folder / "ordinary", shift=shift, extensions=EXTENSIONS)
        check(restore(binary, source, files, folder / "index", shift=shift, indexed=True, extensions=EXTENSIONS) == exact, name)
        result[name] = dict(sha256=exact, live_offsets_excluded=len(files), partition_byte_offset=shift)
        if name in ("ext2-1024", "ext3-4096"):
            result[name]["fallbacks"] = controls(binary, folder, volume, ref, name.startswith("ext3"))
        if name.endswith("-indirect"):
            result[name]["indirect_fallbacks"] = indirect_controls(binary, folder, volume, ref)
        if name == "ext2-1024":
            result[name]["stale_session"] = stale_recovery(binary, folder, volume, ref)
            result[name]["exported_allocation"] = report_allocation(binary, folder, volume, ref)
        if native:
            if name.endswith("-indirect"):
                details = subprocess.check_output(["debugfs", "-R", "stat /saved/sparse-live.bin", volume], text=True, stderr=subprocess.DEVNULL)
                check(all(marker in details for marker in ("(IND):", "(DIND):", "(TIND):", "(70000):", "Links: 2")), details)
                result[name]["native_sparse_live_file"] = "direct,single,double,triple;two hard links"
            result[name]["independent_allocation"] = independent(volume, ref, name.split("-")[0])
        check(hashlib.sha256(volume.read_bytes()).hexdigest() == ref["disk_sha256"], name)
        print(name, len(exact), "actual Linux unlinks matched bytes and SHA-256, live duplicates excluded", flush=True)
    if native:
        result["gpt_4g"] = boundary(binary, root, manifest)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.native)
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8")
