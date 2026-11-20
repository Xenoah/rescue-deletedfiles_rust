"""Discover real Linux MD member superblocks and recover deleted files without manual geometry."""

import argparse
import itertools
import json
import lzma
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

from e2e import check, run
from e2e_imaging import source_with_gaps
from e2e_raid6_layouts import CAPTURES, native_array, restore_known
from verify_camera_raw_metadata import digest


ROOT = CAPTURES.parent / "md-superblocks"


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def unpack(source, dest, checks):
    for name, sha in checks.items():
        file = dest / (name + ".raw")
        with lzma.open(source / (file.name + ".xz")) as archive, file.open("xb") as out:
            shutil.copyfileobj(archive, out)
        check(digest(file) == sha, name)


def arguments(paths, sector):
    return ["raid-md", "--json", "--sector-size", sector,
            *(v for path in reversed(paths) for v in ("--member", path))]


def reject(binary, folder, args):
    output = folder / "invalid.rescue-raid.json"
    result = subprocess.run([binary, *map(str, args), "--output", output], capture_output=True, timeout=120)
    check(result.returncode != 0 and not output.exists(), (args, result.stdout, result.stderr))
    return json.loads(result.stdout or result.stderr)["error"]["code"]


def discover(binary, folder, paths, meta, missing):
    saved = folder / "verified.rescue-raid.json"
    row = run(binary, *arguments(paths, meta["sector_size"]), "--output", saved, timeout=120)
    check(row["metadata_version"] == meta["metadata_version"] and row["array_uuid"] == meta["array_uuid"]
          and row["events"] == meta["events"] and row["missing_slots"] == missing, row)
    cfg = row["config"]
    layout = meta["layout"].lower().replace("-", "_")
    layout = {"parity_first": "pq_first", "parity_last": "pq_last"}.get(layout, layout)
    check(cfg == load(saved) and cfg["level"] == meta["level"] and cfg["layout"] == layout
          and cfg["sector_size"] == meta["sector_size"] and cfg["stripe_bytes"] == meta["stripe_bytes"]
          and len(cfg["members"]) == len(meta["members"]), cfg)
    for name, item in meta["members"].items():
        member = cfg["members"][item["slot"]]
        check(member["length"] == item["component_bytes"], member)
        if item["slot"] in missing:
            check(member["path"] is None, member)
        else:
            check(Path(member["path"]).samefile(folder / (name + ".raw")) and member["offset"] == item["offset"]
                  and member["fingerprint"]["sample_sha256"], member)
    return saved


def mdadm_check(command, folder, meta):
    for name, member in meta["members"].items():
        raw = folder / (name + ".raw")
        text = subprocess.check_output([command, "--examine", "--export", raw], text=True)
        info = dict(line.split("=", 1) for line in text.splitlines() if "=" in line)
        check(info["MD_LEVEL"] == meta["level"] and info["MD_UUID"] == meta["array_uuid"]
              and info["MD_DEV_UUID"] == member["device_uuid"], info)


def repair_checksum(data):
    count = struct.unpack_from("<I", data, 220)[0]
    struct.pack_into("<I", data, 216, 0)
    block = data[:256 + count * 2]
    block += bytes((-len(block)) % 4)
    total = sum(word for (word,) in struct.iter_unpack("<I", block))
    struct.pack_into("<I", data, 216, ((total >> 32) + (total & 0xffffffff)) & 0xffffffff)


def controls(binary, folder, meta, cases, saved):
    paths = [folder / (n + ".raw") for n in "abcde"]
    def refused(selected, sector=512):
        return reject(binary, folder, arguments(selected, sector))
    tests = {"quorum": refused(paths[:2]), "duplicate": refused([paths[0], *paths[:3]])}
    dest = folder / "foreign.raw"
    foreign = CAPTURES.parent / cases["right_symmetric"]["source"] / "a.raw.xz"
    dest.write_bytes(lzma.decompress(foreign.read_bytes()))
    tests["other_array"] = refused([dest, *paths[1:4]])
    pos = meta["members"]["a"]["super_offset"]
    source = paths[0].read_bytes()
    header = source[pos:pos + 4096]
    count = len(meta["members"])
    changes = {
        "magic": (0, "<I", 0), "major": (4, "<I", 2), "feature": (8, "<I", 1),
        "level": (72, "<I", 10), "layout": (76, "<I", 64), "device_number": (160, "<I", 4095),
        "geometry": (92, "<I", count + 1), "events": (200, "<Q", meta["events"] + 1),
        "dirty": (208, "<Q", 0), "unusable_range": (136, "<Q", 1),
        "overlap": (128, "<Q", pos // 512), "wrong_location": (144, "<Q", 0),
        "role_duplicate": (258, "<H", 0), "role_outside": (256, "<H", count),
        "not_active": (256, "<H", 0xffff), "reserved_role": (256, "<H", 0xfffd),
        "unsupported_sector": (128, "<Q", (1 << 64) - 1),
    }
    def altered(block):
        dest.write_bytes(source)
        with dest.open("r+b") as f:
            f.seek(pos)
            f.write(block)
        return refused([dest, *paths[1:4]])
    for label, (offset, fmt, value) in changes.items():
        block = bytearray(header)
        struct.pack_into(fmt, block, offset, value)
        repair_checksum(block)
        tests[label] = altered(block)
    block = bytearray(header)
    block[217] ^= 1
    tests["checksum"] = altered(block)
    block = bytearray(header)
    block[168:184] = bytes.fromhex(meta["members"]["b"]["device_uuid"].replace(":", ""))
    repair_checksum(block)
    tests["duplicate_device_uuid"] = altered(block)
    block = bytearray(header)
    struct.pack_into("<I", block, 220, 2048)
    tests["excess_role_table"] = altered(block)
    dest.write_bytes(source)
    with dest.open("r+b") as f:
        f.write(header)
    tests["two_headers"] = refused([dest, *paths[1:4]])
    dest.write_bytes(source[:pos + 256])
    tests["truncated"] = refused([dest, *paths[1:4]])
    # An odd-sized role table has a final 16-bit contribution to the Linux MD checksum.
    copies = []
    for name, path in zip("abc", paths):
        data = bytearray(path.read_bytes())
        block = bytearray(data[pos:pos + 4096])
        struct.pack_into("<I", block, 220, 7)
        struct.pack_into("<HH", block, 266, 0xffff, 0xffff)
        repair_checksum(block)
        data[pos:pos + 4096] = block
        other = folder / ("odd-" + name + ".raw")
        other.write_bytes(data)
        copies.append(other)
    odd = run(binary, *arguments(copies, 512))
    check(odd["array_uuid"] == meta["array_uuid"] and odd["missing_slots"] == [3, 4], odd)
    tests["odd_roles"] = odd["missing_slots"]
    mapped = source_with_gaps(binary, paths[0], folder / "header-map", [(pos, 512)])
    tests["unread_header"] = refused([mapped, *paths[1:4]])
    tests["readmap_sector_mismatch"] = refused([mapped, *paths[1:4]], 4096)
    # An existing manifest must survive an attempted overwrite, even with valid members.
    original = saved.read_bytes()
    repeat = subprocess.run([binary, *map(str, arguments(paths, 512)), "--output", saved],
                            capture_output=True, timeout=120)
    check(repeat.returncode != 0 and saved.read_bytes() == original, "existing output")
    cfg = load(saved)
    path = Path(next(member["path"] for member in cfg["members"] if member["path"]))
    with path.open("r+b") as f:
        previous = f.read(1)
        f.seek(0)
        f.write(bytes([previous[0] ^ 1]))
    changed = subprocess.run([binary, "raid", "--config", saved, "--json"], capture_output=True, timeout=120)
    check(changed.returncode != 0, changed.stdout)
    tests["changed_after_discovery"] = json.loads(changed.stdout or changed.stderr)["error"]["code"]
    with path.open("r+b") as f:
        f.write(previous)
    return tests


def changed_generation(binary, root, original, native):
    source = ROOT / "changed-generation"
    truth = load(source / "manifest.json")
    with tempfile.TemporaryDirectory(dir=root) as tmp:
        folder = Path(tmp)
        unpack(source, folder, {**truth["members"], "logical": truth["guest_sha256"]})
        unpack(CAPTURES / "left_symmetric", folder, {"a": original["members"]["a"]["sha256"]})
        meta = dict(original, events=truth["events"], guest_sha256=truth["guest_sha256"])
        paths = [folder / (name + ".raw") for name in "abcd"]
        stale = reject(binary, folder, arguments(paths, 512))
        cfg = discover(binary, folder, paths[1:], meta, [0, 4])
        files = restore_known(binary, folder, cfg, original["originals"])
        row = dict(stale=stale, previous_events=truth["previous_events"], current_events=truth["events"], files=files)
        if native:
            row["linux_kernel_array"] = native_array(binary, folder, dict(meta, member_sha256={"logical": truth["guest_sha256"]}), cfg)
        return row


def verify(binary, root, native=False, mdadm="mdadm"):
    cases = load(ROOT / "manifest.json")
    report = {}
    for index, (name, meta) in enumerate(cases.items()):
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            folder = Path(tmp)
            members = meta["members"]
            checks = {key: item["sha256"] for key, item in members.items()}
            checks["logical"] = meta["guest_sha256"]
            unpack(CAPTURES.parent / meta["source"], folder, checks)
            masks = list(itertools.combinations(range(len(members)), 2 if meta["level"] == "raid6" else 1))
            missing = list(masks[index % len(masks)])
            selected = [folder / (n + ".raw") for n, m in members.items() if m["slot"] not in missing]
            cfg = discover(binary, folder, selected, meta, missing)
            row = dict(missing=missing, uuid=meta["array_uuid"], events=meta["events"],
                       files=restore_known(binary, folder, cfg, meta["originals"]))
            if native:
                mdadm_check(mdadm, folder, meta)
                row["linux_kernel_array"] = native_array(binary, folder, dict(meta, member_sha256={"logical": meta["guest_sha256"]}), cfg)
            if name == "left_symmetric":
                row["controls"] = controls(binary, folder, meta, cases, cfg)
            for key, sha in checks.items():
                check(digest(folder / (key + ".raw")) == sha, key)
            report[name] = row
            print(name, len(row["files"]), "unlinks exact", flush=True)
    report["changed_generation"] = changed_generation(binary, root, cases["left_symmetric"], native)
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--mdadm", default="mdadm")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.native, args.mdadm)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
