"""Create VDI snapshots with Oracle VirtualBox and read them again through its native disk API."""

import argparse
import ctypes as c
import hashlib
import json
import lzma
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
MIB = 1 << 20
SECTOR = 512


def sha(data):
    return hashlib.sha256(data).hexdigest()


def require(code):
    if code < 0:
        raise RuntimeError(f"VirtualBox disk API returned {code}")


class VirtualBox:
    def __init__(self, tool):
        self.tool = tool.resolve()
        folder = self.tool.parent
        runtime = c.CDLL(str(folder / "VBoxRT.so"), mode=c.RTLD_GLOBAL)
        disk = c.CDLL(str(folder / "VBoxDDU.so"), mode=c.RTLD_GLOBAL)
        ptr, string, u32 = c.c_void_p, c.c_char_p, c.c_uint32

        def bind(lib, name, *arguments):
            fn = getattr(lib, name)
            fn.argtypes, fn.restype = list(arguments), c.c_int
            return fn

        init = bind(runtime, "RTR3InitEx", u32, u32, c.c_int, ptr, string)
        require(init(2, 0, 0, None, bytes(self.tool)))
        self.create = bind(disk, "VDCreate", ptr, c.c_int, c.POINTER(ptr))
        self.open = bind(disk, "VDOpen", ptr, string, string, u32, ptr)
        self.diff = bind(
            disk,
            "VDCreateDiff",
            ptr,
            string,
            string,
            u32,
            string,
            ptr,
            ptr,
            u32,
            ptr,
            ptr,
        )
        self.write = bind(disk, "VDWrite", ptr, c.c_uint64, ptr, c.c_size_t)
        self.read = bind(disk, "VDRead", ptr, c.c_uint64, ptr, c.c_size_t)
        self.flush = bind(disk, "VDFlush", ptr)
        self.destroy = bind(disk, "VDDestroy", ptr)
        self.shutdown = bind(disk, "VDShutdown")

    def handle(self):
        disk = c.c_void_p()
        require(self.create(None, 1, c.byref(disk)))  # VDTYPE_HDD
        return disk

    def write_changes(self, disk, previous, current):
        for pos in range(0, len(current), MIB):
            old, new = previous[pos : pos + MIB], current[pos : pos + MIB]
            if new == old:
                continue
            if not any(new):
                require(self.write(disk, pos, new, len(new)))
                continue
            start = None
            for off in range(0, len(new) + SECTOR, SECTOR):
                if (
                    off < len(new)
                    and old[off : off + SECTOR] != new[off : off + SECTOR]
                ):
                    if start is None:
                        start = off
                elif start is not None:
                    require(self.write(disk, pos + start, new[start:off], off - start))
                    start = None
        require(self.flush(disk))

    def capture(self, members, guests, fixed, tmp):
        raw = tmp / "base.raw"
        raw.write_bytes(guests[0])
        subprocess.run(
            [
                self.tool,
                "convert",
                "--srcformat",
                "RAW",
                "--dstformat",
                "VDI",
                "--variant",
                "Fixed" if fixed else "Standard",
                "--srcfilename",
                raw,
                "--dstfilename",
                members[0],
            ],
            check=True,
        )
        raw.unlink()
        disk = self.handle()
        try:
            require(self.open(disk, b"VDI", bytes(members[0]), 0, None))
            for i, child in enumerate(members[1:], 1):
                require(
                    self.diff(
                        disk, b"VDI", bytes(child), 0, None, None, None, 0, None, None
                    )
                )
                self.write_changes(disk, guests[i - 1], guests[i])
        finally:
            require(self.destroy(disk))

        # Reopen a fresh read-only container for each generation.
        for i, guest in enumerate(guests):
            disk = self.handle()
            try:
                for member in members[: i + 1]:
                    require(self.open(disk, b"VDI", bytes(member), 1, None))
                for pos in range(0, len(guest), MIB):
                    expected = guest[pos : pos + MIB]
                    buf = c.create_string_buffer(len(expected))
                    require(self.read(disk, pos, buf, len(expected)))
                    assert buf.raw == expected
            finally:
                require(self.destroy(disk))


def fat_snapshots(tmp, out):
    disk = tmp / "fat32.raw"
    with disk.open("wb") as image:
        image.truncate(40 * MIB)

    def command(*args):
        return subprocess.check_output(list(map(str, args)))

    command("mkfs.fat", "-F", "32", "-s", "1", disk)
    files = sorted(
        (
            ROOT / "tests/corpus/files" / ("sample." + ext)
            for ext in ("bmp", "gif", "pdf", "png", "wav", "webp", "zip")
        )
    )
    for src in files:
        command("mcopy", "-i", disk, src, "::/" + src.name)
        assert command("mtype", "-i", disk, "::/" + src.name) == src.read_bytes()
    # Two trailing, unpartitioned witness blocks exercise explicit zero and inheritance.
    tail = bytes([67]) * MIB + bytes([129]) * MIB
    stages = [disk.read_bytes() + tail]
    for group in (files[:3], files[3:]):
        for src in group:
            command("mdel", "-i", disk, "::/" + src.name)
        listing = command("mdir", "-b", "-i", disk, "::/").lower()
        assert all(src.name.encode() not in listing for src in group)
        tail = (
            bytes(MIB) + bytes([129]) * MIB
            if len(stages) == 1
            else bytes(MIB)
            + bytes([129]) * 512
            + bytes([44]) * 512
            + bytes([129]) * (MIB - 1024)
        )
        stages.append(disk.read_bytes() + tail)
    for i, data in enumerate(stages):
        (out / f"fat-{i}.raw.xz").write_bytes(lzma.compress(data, preset=3))
    return stages, {p.name: sha(p.read_bytes()) for p in files}


def generate(box, target):
    target.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for case in (
        "fixed-child",
        "dynamic-two-children",
        "dynamic-flushed",
        "fat32-zero",
    ):
        out = target / case
        out.mkdir()
        with tempfile.TemporaryDirectory() as temp:
            tmp = Path(temp)
            if case == "fat32-zero":
                guests, originals = fat_snapshots(tmp, out)
            else:
                source = ROOT / "tests/vhd-chain" / case
                metadata = json.loads((source / "manifest.json").read_text())
                originals = metadata["originals"]
                guests = [
                    lzma.decompress(
                        (source / (m["filename"] + ".guest.raw.xz")).read_bytes()
                    )
                    for m in metadata["members"]
                ]
                assert [sha(g) for g in guests] == [
                    m["guest_sha256"] for m in metadata["members"]
                ]
            members = [
                tmp / ("base.vdi" if i == 0 else f"child{i}.vdi")
                for i in range(len(guests))
            ]
            box.capture(members, guests, case == "fixed-child", tmp)
            records = []
            for image, guest in zip(members, guests, strict=True):
                data = image.read_bytes()
                map_at = struct.unpack_from("<I", data, 340)[0]
                count = struct.unpack_from("<I", data, 384)[0]
                slots = struct.unpack_from(f"<{count}I", data, map_at)
                (out / (image.name + ".xz")).write_bytes(lzma.compress(data, preset=3))
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
            manifest[case] = dict(members=records, originals=originals)
            print(
                case,
                [(m["data"], m["zero"], m["inherited"]) for m in records],
                flush=True,
            )
    (target / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vbox-img", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    box = VirtualBox(args.vbox_img)
    try:
        generate(box, args.output)
    finally:
        require(box.shutdown())
