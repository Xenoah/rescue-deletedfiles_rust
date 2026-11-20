"""Capture seven real ext4 unlinks in seven consecutive native VirtualBox snapshots."""

import argparse
import json
import lzma
from pathlib import Path
import re
import struct
import subprocess
import tempfile

from capture import MIB, ROOT, VirtualBox, require, sha


def make_snapshots(tmp, target):
    source = tmp / "ext4.img"
    with source.open("xb") as f:
        f.truncate(16 * MIB)
    subprocess.run(["mkfs.ext4", "-q", "-F", "-b", "4096", source], check=True)

    def debug(command, write=False):
        subprocess.run(
            ["debugfs", *(["-w"] if write else []), "-R", command, source],
            check=True,
            capture_output=True,
        )

    originals = {}
    for ext in ("bmp", "gif", "pdf", "png", "wav", "webp", "zip"):
        p = ROOT / "tests/corpus/files" / ("sample." + ext)
        debug(f"write {p} /{p.name}", True)
        readback = tmp / ("before-" + p.name)
        debug(f"dump /{p.name} {readback}")
        assert readback.read_bytes() == p.read_bytes()
        originals[p.name] = sha(p.read_bytes())
    # A sector outside ext4 in each newly allocated block proves that even the oldest
    # of the eight layers is needed to assemble the youngest guest.
    tail = bytearray(8 * MIB)
    tail[:512] = b"B" * 512
    guests = []
    receipts = []
    members = [None, *originals.keys()]
    for index, name in enumerate(members):
        if name:
            debug("rm /" + name, True)
            tail[index * MIB : index * MIB + 512] = bytes([65 + index]) * 512
        rows = subprocess.check_output(["fls", "-f", "ext4", "-d", source], text=True)
        entries = dict(
            (n, int(ino))
            for ino, n in re.findall(r"^r/r \* (\d+):\s+(sample\.\w+)$", rows, re.M)
        )
        assert set(entries) == set(members[1 : index + 1]), (entries, index)
        for n, ino in entries.items():
            actual = subprocess.check_output(
                ["icat", "-r", "-f", "ext4", source, str(ino)]
            )
            assert (
                actual == (ROOT / "tests/corpus/files" / n).read_bytes()
                and sha(actual) == originals[n]
            )
        guest = source.read_bytes() + tail
        guests.append(guest)
        (target / f"ext4-{index}.raw.xz").write_bytes(lzma.compress(guest, preset=3))
        receipts.append(entries)
    return guests, originals, receipts


def generate(box, target):
    target.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory() as temp:
        tmp = Path(temp)
        guests, originals, deleted = make_snapshots(tmp, target)
        members = [tmp / ("base.vdi" if i == 0 else f"child{i}.vdi") for i in range(8)]
        box.capture(members, guests, False, tmp)
        records = []
        for i, (image, guest) in enumerate(zip(members, guests, strict=True)):
            data = image.read_bytes()
            (target / (image.name + ".xz")).write_bytes(lzma.compress(data, preset=3))
            map_at = struct.unpack_from("<I", data, 340)[0]
            count = struct.unpack_from("<I", data, 384)[0]
            slots = struct.unpack_from(f"<{count}I", data, map_at)
            assert slots[16 + i] < 0xFFFFFFFE
            if i:
                assert slots[16 + i - 1] == 0xFFFFFFFF
            records.append(
                dict(
                    filename=image.name,
                    sha256=sha(data),
                    guest_bytes=len(guest),
                    guest_sha256=sha(guest),
                    inherited=slots.count(0xFFFFFFFF),
                    zero=slots.count(0xFFFFFFFE),
                    data=sum(v < 0xFFFFFFFE for v in slots),
                )
            )
        (target / "manifest.json").write_text(
            json.dumps(
                dict(members=records, originals=originals, deleted=deleted),
                sort_keys=True,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        print(
            "8 distinct generations; actual deleted ext4 file counts",
            [len(r) for r in deleted],
        )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--vbox-img", required=True, type=Path)
    p.add_argument("--output", required=True, type=Path)
    a = p.parse_args()
    v = VirtualBox(a.vbox_img)
    try:
        generate(v, a.output)
    finally:
        require(v.shutdown())
