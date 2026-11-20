"""Reconstruct genuine Linux LVM2 linear/striped volumes, then recover actual ext4 unlinks."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

from e2e import check, run

GOLD = Path(__file__).resolve().parents[1] / "tests/lvm2"
FILES = GOLD.parent / "corpus/files"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def unpack(folder, out, captures):
    for name, proof in captures.items():
        data = lzma.decompress((folder / (name + ".raw.xz")).read_bytes())
        check(len(data) == proof["bytes"] and sha(data) == proof["sha256"], name)
        (out / (name + ".raw")).write_bytes(data)


def source_args(images):
    return ["--source", images[0], *[arg for path in images[1:] for arg in ("--segment", path)]]


def recover(binary, disk, root, proof, extra):
    scan = run(binary, "scan", "--source", disk, "--work-dir", root, "--mode", "carve", *extra, "--json")
    rows = run(binary, "results", "--session", scan["session"], "--json")["candidates"]
    truth = {h: name for name, h in proof["matched"].items()}
    ids = {row["record_sha256"]: row["id"] for row in rows if row.get("record_sha256") in truth}
    check(ids.keys() == truth.keys(), (root, ids.keys(), truth.keys()))
    flags = [v for x in ids.values() for v in ("--id", x)]
    restored = run(binary, "recover", "--session", scan["session"], *flags, "--destination", root / "out", "--json")
    for item in restored["results"]:
        source = FILES / truth[item["sha256"]]
        data = Path(item["path"]).read_bytes()
        check(item["status"] == "completed" and data == source.read_bytes() and sha(data) == item["sha256"], source)
    check(len(restored["results"]) == len(truth), restored)


def real(binary, root, folder):
    proof = json.loads((folder / "manifest.json").read_text(encoding="utf-8"))
    root.mkdir()
    unpack(folder, root, proof["captures"])
    images = [root / "pv-a.raw", root / "pv-b.raw"]
    rows = run(binary, "lvm-volumes", *source_args(images), "--json")["volumes"]
    check({r["name"] for r in rows} == set(proof["volumes"]), rows)
    check(run(binary, "lvm-volumes", *source_args(images[::-1]), "--json")["volumes"] == rows, rows)
    result = {}
    for r in rows:
        name = r["name"]
        expected = proof["volumes"][name]
        disk = root / (name + ".raw")
        check(r["group"] == "rescue_vg" and r["bytes"] == disk.stat().st_size, r)
        for key, selector, members in [("ordinary", name, images), ("index", r["id"], images[::-1])]:
            output = root / (key + "-" + name + ".img")
            report = run(binary, "import-image", "--image-type", "lvm2", *source_args(members),
                         "--lvm-volume", selector, "--raw-bytes", str(r["bytes"]),
                         "--raw-sha256", expected["volume_sha256"], "--output", output, "--json")
            check(output.read_bytes() == disk.read_bytes() and sha(output.read_bytes()) == expected["volume_sha256"]
                  and report["sha256"] == expected["volume_sha256"] and report["bytes"] == r["bytes"], name)
            recover(binary, output, root / (name + "-" + key), expected, [] if key == "ordinary" else ["--indexed"])
            output.unlink()
        result[name] = {**r, **expected}
        print(folder.name, name, len(expected["matched"]), "kernel ext4 unlinks and whole LV exact", flush=True)
    for name, p in zip(("pv-a", "pv-b"), images):
        check(sha(p.read_bytes()) == proof["captures"][name]["sha256"], p)
    return result


def checksum(data):
    return zlib.crc32(data, 0xf597a6cf ^ 0xffffffff) ^ 0xffffffff


def apply_text(data, transform):
    # The basic fixture has one MDA on each physical volume.
    start = 4096
    where, size = struct.unpack_from("<QQ", data, start + 40)
    text = bytes(data[start + where:start + where + size])
    new = transform(text)
    check(new != text and len(new) < (16 << 10), "control edit")
    data[start + where:start + where + len(new)] = new
    struct.pack_into("<QI", data, start + 48, len(new), checksum(new))
    struct.pack_into("<I", data, start, checksum(data[start + 4:start + 512]))


def controls(binary, root):
    folder = root / "extent8-stripe64"
    pvs = [folder / "pv-a.raw", folder / "pv-b.raw"]
    rejected = []
    def refuse(label, paths, selector="linear", extra=()):
        output = root / ("refused-" + label + ".raw")
        p = subprocess.run([binary, "import-image", "--image-type", "lvm2", *source_args(paths),
                            "--lvm-volume", selector, "--output", output, *extra, "--json"], capture_output=True)
        check(p.returncode != 0 and not output.exists(), (label, p.returncode, p.stdout))
        rejected.append(label)

    refuse("missing-pv", pvs[:1])
    refuse("duplicate-pv", [pvs[0], pvs[0]])
    refuse("unknown-volume", pvs, "absent")
    refuse("logical-digest", pvs, extra=["--raw-sha256", "00" * 32])
    refuse("logical-length", pvs, extra=["--raw-bytes", "17"])
    modified = [root / "changed-a.raw", root / "changed-b.raw"]
    for label, offset in [("label", 513), ("label-crc", 528), ("mda-crc", 4096), ("mda-signature", 4100)]:
        data = bytearray(pvs[0].read_bytes())
        data[offset] ^= 1
        modified[0].write_bytes(data)
        refuse(label, [modified[0], pvs[1]])
    data = bytearray(pvs[0].read_bytes())
    offset = 4096 + struct.unpack_from("<Q", data, 4136)[0]
    data[offset + 20] ^= 1
    modified[0].write_bytes(data)
    refuse("metadata-text-crc", [modified[0], pvs[1]])
    for label, old, new in [
        ("double-seqno", b"seqno = 4", b"seqno = 4\nseqno = 5"),
        ("overlap", b'stripes = [\n"pv1", 0', b'stripes = [\n"pv0", 0'),
        ("past-pv-end", b'"pv1", 8', b'"pv1", 31'),
        ("gap-in-lv", b"start_extent = 8", b"start_extent = 9"),
        ("extent-count", b"segment_count = 2", b"segment_count = 1"),
        ("invalid-stripe-count", b"stripe_count = 2", b"stripe_count = 0"),
        ("invalid-stripe-size", b"stripe_size = 128", b"stripe_size = 192"),
        ("unknown-pv", b'"pv1", 8', b'"pv9", 8'),
        ("snapshot", b'type = "striped"', b'type = "snapshot"'),
        ("thin", b'type = "striped"', b'type = "thin"'),
        ("raid", b'type = "striped"', b'type = "raid1"'),
        ("partial-group", b'["RESIZEABLE", "READ", "WRITE"]', b'["PARTIAL", "READ", "WRITE"]'),
    ]:
        for source, path in zip(pvs, modified):
            data = bytearray(source.read_bytes())
            apply_text(data, lambda x: x.replace(old, new, 1))
            path.write_bytes(data)
        refuse(label, modified)
    data = bytearray(pvs[0].read_bytes())
    apply_text(data, lambda x: x.replace(b"seqno = 4", b"seqno = 3"))
    modified[0].write_bytes(data)
    refuse("metadata-disagreement", [modified[0], pvs[1]])

    # A valid data alteration cannot be attributed to a bad metadata checksum.
    data = bytearray(pvs[0].read_bytes())
    data[(1 << 20) + 100] ^= 1
    modified[0].write_bytes(data)
    proof = json.loads((GOLD / "extent8-stripe64/manifest.json").read_text(encoding="utf-8"))
    refuse("changed-valid-data", [modified[0], pvs[1]], extra=["--raw-sha256", proof["volumes"]["linear"]["volume_sha256"]])
    other = root / "dual-mda-extent3-stripe16"
    if other.exists():
        refuse("unrelated-group", [pvs[0], other / "pv-b.raw"])
        other_a = bytearray((other / "pv-a.raw").read_bytes())
        second, = struct.unpack_from("<Q", other_a, 632)
        other_a[second + 4] ^= 1
        modified[0].write_bytes(other_a)
        refuse("second-metadata-copy", [modified[0], other / "pv-b.raw"])
    return sorted(rejected)


def verify(binary, root):
    cases = {folder.name: real(binary, root / folder.name, folder)
             for folder in sorted(GOLD.iterdir()) if (folder / "manifest.json").exists()}
    return {"layouts": cases, "refused": controls(binary, root)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp))
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8")
