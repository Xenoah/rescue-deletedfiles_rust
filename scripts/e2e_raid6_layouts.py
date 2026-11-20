"""Recover genuine ext4 unlinks from five-member Linux MD RAID6 arrays with different P/Q layouts."""

import argparse
import hashlib
import itertools
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check, run
from e2e_imaging import source_with_gaps
from e2e_known_extents import CORPUS
from verify_camera_raw_metadata import digest, equal

CAPTURES = CORPUS.parents[1] / "raid6-linux"
SLOTS = "abcde"


def unpack(case, folder, meta):
    for name, checksum in meta["member_sha256"].items():
        dest = folder / (name + ".raw")
        with lzma.open(case / (dest.name + ".xz")) as source, dest.open("xb") as target:
            shutil.copyfileobj(source, target)
        check(digest(dest) == checksum, name)


def manifest(folder, meta, layout, missing):
    config = dict(schema_version=1, level="raid6", layout=layout, sector_size=512,
                  stripe_bytes=meta["stripe_bytes"], members=[
                      dict(path=None if i in missing else str(folder / (letter + ".raw")),
                           offset=meta["data_offset"], length=meta["component_bytes"])
                      for i, letter in enumerate(SLOTS)
                  ])
    path = folder / "array.rescue-raid.json"
    path.write_text(json.dumps(config), encoding="utf-8")
    return path


def extents(binary, folder, config, offsets, original, reject=False):
    plan, out = folder / "plan.json", folder / "recovered.file"
    checksum = hashlib.sha256(original).hexdigest()
    plan.write_text(json.dumps(dict(schema_version=1, sha256=checksum, extents=offsets)), encoding="utf-8")
    cmd = ["recover-extents", "--source", config, "--layout", plan, "--output", out, "--json"]
    if reject:
        res = subprocess.run([binary, *cmd], capture_output=True, timeout=90)
        check(res.returncode != 0 and not out.exists(), (config, res.stdout, res.stderr))
        return json.loads(res.stdout or res.stderr)["error"]["code"]
    row = run(binary, *cmd, timeout=90)
    check(row["original_sha256_verified"] and row["file"]["sha256"] == digest(out) == checksum
          and out.read_bytes() == original, config)
    out.unlink()
    return checksum


def restore_known(binary, folder, cfg, files):
    hashes = {}
    for name, entry in files.items():
        data = (CORPUS / name).read_bytes()
        hashes[name] = extents(binary, folder, cfg, [dict(offset=entry["offset"], length=len(data))], data)
        check(hashes[name] == entry["sha256"], name)
    return hashes


def samples(folder, meta):
    # Unique, nonzero sectors from every chunk in two complete P/Q rotations.
    start = meta["marker_offset"]
    chunk = meta["stripe_bytes"]
    guest = (folder / "logical.raw").read_bytes()
    first = (start + chunk - 1) // chunk * chunk
    ranges = [dict(offset=first + i * chunk + 71, length=257) for i in range(30)]
    check(ranges[-1]["offset"] + 257 <= start + meta["marker_bytes"], ranges)
    parts = [guest[v["offset"]:v["offset"]+v["length"]] for v in ranges]
    check(len(set(parts)) == len(parts) and all(any(piece) for piece in parts), "nonunique parity test")
    return ranges, b"".join(parts)


def native_array(binary, folder, meta, cfg):
    row = run(binary, "image", "--source", cfg, "--destination", folder / "acquired",
              "--block-size", 1048576, "--retries", 0, "--json", timeout=300)
    image = Path(row["image"])
    check(row["state"] == "completed" and digest(image) == meta["member_sha256"]["logical"]
          and equal(image, folder / "logical.raw"), row)
    session = run(binary, "scan", "--source", image, "--mode", "carve", "--work-dir", folder / "scan",
                  "--json", timeout=180)["session"]
    candidates = run(binary, "results", "--session", session, "--limit", 1000, "--json")["candidates"]
    originals = {value["sha256"]: name for name, value in meta["originals"].items()}
    chosen = {r["record_sha256"]:r["id"] for r in candidates if r.get("record_sha256") in originals}
    check(chosen.keys() == originals.keys(), "carving missed real deleted files")
    flags = [arg for ident in chosen.values() for arg in ("--id", ident)]
    rows = run(binary, "recover", "--session", session, *flags, "--destination", folder / "restored",
               "--json", timeout=180)["results"]
    checked = {originals[r["sha256"]]: r["sha256"] for r in rows if equal(Path(r["path"]), CORPUS / originals[r["sha256"]])}
    check(len(checked) == len(originals), checked)
    return dict(guest_sha256=digest(image), bytes=image.stat().st_size, carved=checked)


def verify(binary, root, native=False, mdadm="mdadm"):
    records = json.loads((CAPTURES / "manifest.json").read_text(encoding="utf-8"))
    result = {}
    for number, (layout, meta) in enumerate(records.items()):
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            folder = Path(tmp)
            unpack(CAPTURES / layout, folder, meta)
            ranges, reference = samples(folder, meta)
            pairs = list(itertools.combinations(range(5), 2))
            masks = [(), *((i,) for i in range(5)), *pairs]
            checks = {}
            for mask in masks:
                cfg = manifest(folder, meta, layout, mask)
                key = "".join(SLOTS[i] for i in mask) or "all-present"
                checks[key] = extents(binary, folder, cfg, ranges, reference)
            missing = pairs[number % len(pairs)]
            cfg = manifest(folder, meta, layout, missing)
            if layout == "ddf_n_continue":
                options = run(binary, "raid", "--config", cfg, "--search", "--stripe-sizes", meta["stripe_bytes"],
                              "--max-configurations", 14, "--max-milliseconds", 30000, "--json", timeout=90)
                check(options["tested_configurations"] == 14, options)
            saved = restore_known(binary, folder, cfg, meta["originals"])
            entry = dict(missing_for_file_recovery=list(missing), originals=saved, parity_probes=checks)
            if native:
                ids = set()
                for slot in SLOTS:
                    p = subprocess.run([mdadm, "--examine", "--export", folder / (slot + ".raw")],
                                       capture_output=True, check=True, text=True)
                    info = dict(line.split("=",1) for line in p.stdout.splitlines() if "=" in line)
                    check(info["MD_LEVEL"] == "raid6" and info["MD_DEVICES"] == "5", info)
                    ids.add(info["MD_UUID"])
                check(len(ids) == 1, ids)
                entry["linux_kernel_array"] = native_array(binary, folder, meta, cfg)
            cfg = manifest(folder, meta, layout, (0, 2, 4))
            entry["three_missing"] = extents(binary, folder, cfg, ranges, reference, reject=True)
            cfg = manifest(folder, meta, layout, ())
            # Damage every byte in one physical chunk from a sampled rotation.
            raw = folder / "c.raw"
            stripe = ranges[3]["offset"] // (3*meta["stripe_bytes"])
            pos = meta["data_offset"] + stripe*meta["stripe_bytes"]
            with raw.open("r+b") as f:
                f.seek(pos); block=f.read(meta["stripe_bytes"])
                f.seek(pos); f.write(bytes(v ^ 0x5a for v in block))
            entry["contradictory_parity"] = extents(binary, folder, cfg, ranges, reference, reject=True)
            # Two absent slots consume both independent parity equations. A bad
            # surviving sector can now reconstruct to other bytes of the same length.
            cfg = manifest(folder, meta, layout, (0, 1))
            entry["silent_change_wrong_digest"] = extents(binary, folder, cfg, ranges, reference, reject=True)
            with raw.open("r+b") as f:
                f.seek(pos); f.write(block)
            if layout in {"left_asymmetric", "ddf_n_continue", "right_symmetric_6"}:
                # One whole member is absent; an independently captured read error affects
                # only this stripe on another member. Adding a third gap must remain local.
                mapped = source_with_gaps(binary, raw, folder / "member-map", [(pos, meta["stripe_bytes"])])
                for unavailable, reject in [((0,), False), ((0, 1), True)]:
                    cfg = manifest(folder, meta, layout, unavailable)
                    config = json.loads(cfg.read_text(encoding="utf-8"))
                    config["members"][2]["path"] = str(mapped)
                    cfg.write_text(json.dumps(config), encoding="utf-8")
                    key = "third_read_error" if reject else "second_read_error"
                    entry[key] = extents(binary, folder, cfg, ranges, reference, reject=reject)
                tail = ranges[-1:]
                n = tail[0]["length"]
                entry["outside_third_gap"] = extents(binary, folder, cfg, tail, reference[-n:])
            if layout in {"left_asymmetric", "right_symmetric", "left_asymmetric_6"}:
                other = {"left_asymmetric":"left_symmetric", "right_symmetric":"right_asymmetric",
                         "left_asymmetric_6":"left_symmetric_6"}[layout]
                cfg = manifest(folder, meta, other, ())
                entry["wrong_logical_order"] = extents(binary, folder, cfg, ranges, reference, reject=True)
            for name, original in meta["member_sha256"].items():
                check(digest(folder / (name + ".raw")) == original, name)
            result[layout] = entry
            print(layout, len(saved), "real unlinks exact; 16 availability patterns and corruption checks", flush=True)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--mdadm", default="mdadm")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        report = verify(args.binary.resolve(), Path(tmp), args.native, args.mdadm)
    if args.output:
        args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
