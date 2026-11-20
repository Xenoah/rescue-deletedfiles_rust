"""Recover ext4 unlinks after discovering genuine stopped Linux MD mirrors and near-two RAID10."""

import argparse
import itertools
import json
import tempfile
from pathlib import Path

from e2e import check, run
from e2e_imaging import source_with_gaps
from e2e_md_superblocks import ROOT as PARITY, arguments, discover, load, mdadm_check, reject, unpack
from e2e_raid6_layouts import extents, native_array, restore_known
from verify_camera_raw_metadata import digest


ROOT = PARITY.parent / "md-mirrors"


def sample_ranges(folder, meta):
    guest = (folder / "logical.raw").read_bytes()
    unit = 16384
    first = (meta["marker_offset"] + unit - 1) // unit * unit
    sections = [dict(offset=first + n * unit + 39, length=99) for n in range(30)]
    pieces = [guest[p["offset"]:p["offset"] + p["length"]] for p in sections]
    check(all(any(b) for b in pieces) and len(set(pieces)) == 30, "marker samples repeat")
    return sections, b"".join(pieces)


def disagreement(binary, folder, meta):
    # Damage a data byte on one of two surviving mirror copies. The oracle
    # addresses a logical byte supplied by that copy's actual mirror pair.
    group = meta["level"] == "raid10"
    paths = [folder / (n + ".raw") for n in meta["members"]]
    unit = meta["stripe_bytes"]
    rotation = 2 if group else 1
    logical = ((meta["marker_offset"] + unit * rotation - 1) // (unit * rotation)) * unit * rotation
    offset = meta["members"]["a"]["offset"] + (logical // rotation)
    original = (folder / "logical.raw").read_bytes()[logical:logical + 512]
    with paths[0].open("r+b") as f:
        f.seek(offset)
        block = f.read(512)
        check(block == original and any(block), "wrong physical mirror offset")
        f.seek(offset)
        f.write(bytes(x ^ 0xd5 for x in block))
    cfg = folder / "divergent.rescue-raid.json"
    run(binary, *arguments(paths, meta["sector_size"]), "--output", cfg, timeout=120)
    ranges = [dict(offset=logical, length=len(original))]
    result = {"divergent": extents(binary, folder, cfg, ranges, original, reject=True)}
    alone = folder / "silent.rescue-raid.json"
    run(binary, *arguments([paths[0], *([paths[2]] if group else [])], meta["sector_size"]),
        "--output", alone, timeout=120)
    result["wrong_only_copy"] = extents(binary, folder, alone, ranges, original, reject=True)
    healthy = folder / "healthy.rescue-raid.json"
    run(binary, *arguments([paths[1], *([paths[2]] if group else [])], meta["sector_size"]),
        "--output", healthy, timeout=120)
    result["healthy_copy"] = extents(binary, folder, healthy, ranges, original)
    with paths[0].open("r+b") as f:
        f.seek(offset)
        f.write(block)
    if meta["sector_size"] == 512:
        mapped = source_with_gaps(binary, paths[0], folder / "data-map", [(offset, len(block))])
        target = folder / "mapped.rescue-raid.json"
        run(binary, *arguments([mapped, *paths[1:]], 512), "--output", target, timeout=120)
        result["other_copy_covers_gap"] = extents(binary, folder, target, ranges, original)
    return result


def verify(binary, root, native=False, mdadm="mdadm"):
    results = {}
    for name, meta in load(ROOT / "manifest.json").items():
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            folder = Path(tmp)
            checks = {n: m["sha256"] for n, m in meta["members"].items()}
            checks["logical"] = meta["guest_sha256"]
            unpack(ROOT / name, folder, checks)
            paths = [folder / (n + ".raw") for n in meta["members"]]
            slots = range(len(paths))
            if native:
                mdadm_check(mdadm, folder, meta)
            if not meta["supported"]:
                results[name] = {"unsupported_layout": reject(binary, folder, arguments(paths, meta["sector_size"]))}
                continue
            sections, reference = sample_ranges(folder, meta)
            masks = {}
            for count in range(1, len(paths) + 1):
                for active in itertools.combinations(slots, count):
                    present = [paths[i] for i in active]
                    covered = meta["level"] == "raid1" or all(
                        i in active or i + 1 in active for i in range(0, len(paths), 2))
                    key = "".join("abcde"[i] for i in active)
                    if covered:
                        cfg = discover(binary, folder, present, meta, [i for i in slots if i not in active])
                        masks[key] = extents(binary, folder, cfg, sections, reference)
                        cfg.unlink()
                    else:
                        masks[key] = reject(binary, folder, arguments(present, meta["sector_size"]))
            active = [len(paths) - 1] if meta["level"] == "raid1" else [1, 2]
            cfg = discover(binary, folder, [paths[i] for i in active], meta, [i for i in slots if i not in active])
            row = dict(active=active, availability=masks, files=restore_known(binary, folder, cfg, meta["originals"]))
            if native:
                row["linux_kernel_array"] = native_array(binary, folder, dict(meta, member_sha256={"logical": meta["guest_sha256"]}), cfg)
            row["integrity"] = disagreement(binary, folder, meta)
            for key, sha in checks.items():
                check(digest(folder / (key + ".raw")) == sha, key)
            results[name] = row
            print(name, len(masks), "survivor selections; ten deleted files exact", flush=True)
    return results


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
