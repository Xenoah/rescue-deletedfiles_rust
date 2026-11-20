"""Capture clean kernel-unmounted XFS images with actual unlinks and identical live controls."""

from contextlib import contextmanager
import argparse
import ctypes
import os
import struct
import hashlib
import json
import lzma
import mmap
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
FILES = {
    n: (ROOT.parent / "corpus/files" / n).read_bytes()
    for n in [
        "sample.bmp",
        "sample.gif",
        "sample.png",
        "sample.pdf",
        "sample.webp",
        "sample.wav",
        "sample.zip",
    ]
}


def cmd(*args):
    return subprocess.run(
        list(map(str, args)), check=True, timeout=300, capture_output=True, text=True
    ).stdout


@contextmanager
def mounted(disk, directory):
    cmd("mount", "-t", "xfs", "-o", "loop,nodiscard", disk, directory)
    try:
        yield
    finally:
        cmd("umount", directory)


def hits(image, value):
    result = []
    pos = image.find(value)
    while pos != -1:
        result.append(pos)
        pos = image.find(value, pos + 1)
    return result


def fragmented_log(mount, block):
    """Make both free-space trees grow and force committed metadata through the circular log."""
    path = mount / "fragmented"
    with path.open("w+b", buffering=0) as f:
        os.posix_fallocate(f.fileno(), 0, block * 4096)
        punch = ctypes.CDLL(None, use_errno=True).fallocate
        punch.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_longlong, ctypes.c_longlong]
        for nr in range(1, 4096, 2):
            if punch(f.fileno(), 3, nr * block, block):
                raise OSError(ctypes.get_errno(), "punch file range")
        for rev in range(18000):
            os.setxattr(f.fileno(), "user.revision", struct.pack(">Q", rev))
            os.fsync(f.fileno())


def generate(root, block, sector, reflink, ags, fragmented=False):
    disk = root / "volume.raw"
    mount = root / "mount"
    mount.mkdir()
    with disk.open("xb") as f:
        f.truncate(384 << 20)
    cmd(
        "mkfs.xfs",
        "-f",
        "-K",
        "-b",
        f"size={block}",
        "-s",
        f"size={sector}",
        "-m",
        f"crc=1,reflink={int(reflink)},rmapbt={int(reflink)}",
        "-d",
        f"agcount={ags}",
        disk,
    )
    with mounted(disk, mount):
        if fragmented:
            fragmented_log(mount, block)
        (mount / "deleted").mkdir()
        (mount / "live").mkdir()
        for n, v in FILES.items():
            p = mount / "deleted" / n
            p.write_bytes(v)
            assert p.read_bytes() == v
    with disk.open("rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as image:
        original = {n: hits(image, v) for n, v in FILES.items()}
    assert all(len(v) == 1 for v in original.values()), original
    with mounted(disk, mount):
        for n, v in FILES.items():
            p = mount / "live" / n
            p.write_bytes(v)
            assert p.read_bytes() == v
        for n in FILES:
            (mount / "deleted" / n).unlink()
        assert not list((mount / "deleted").iterdir()) and sorted(
            p.name for p in (mount / "live").iterdir()
        ) == sorted(FILES)
    cmd("xfs_repair", "-n", "-m", "256", disk)
    info = cmd("xfs_info", disk)
    groups = cmd("xfs_db", "-r", "-c", "sb 0", "-c", "p agblocks", disk)
    agblocks = int(re.search(r"agblocks = (\d+)", groups)[1])
    dump = cmd("xfs_db", "-r", "-c", "freesp -d", disk)
    extents = [
        tuple(map(int, line.split()))
        for line in dump.splitlines()
        if re.fullmatch(r"\s*\d+\s+\d+\s+\d+\s*", line)
    ]
    assert extents, dump

    def free(blocknr):
        ag, no = divmod(blocknr, agblocks)
        return any(group == ag and start <= no < start + length for group, start, length in extents)

    vm = {}
    with disk.open("rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as image:
        for name, data in FILES.items():
            offsets = hits(image, data)
            (removed,) = original[name]
            assert len(offsets) == 2 and removed in offsets, (name, offsets, removed)
            (live,) = set(offsets) - {removed}
            for loc, unused in [(removed, True), (live, False)]:
                assert all(
                    free(i) == unused
                    for i in range(loc // block, (loc + len(data) - 1) // block + 1)
                ), (name, loc, unused)
            vm[name] = {
                "sha256": hashlib.sha256(data).hexdigest(),
                "bytes": len(data),
                "deleted_offset": removed,
                "live_offset": live,
            }
        disk_hash = hashlib.sha256(image).hexdigest()
    dest = ROOT / f"xfs-{block}-{sector}-{ags}.raw.xz"
    with disk.open("rb") as f, lzma.open(dest, "wb", preset=4) as z:
        shutil.copyfileobj(f, z, length=1 << 20)
    return {
        "block_bytes": block,
        "sector_bytes": sector,
        "allocation_groups": ags,
        "agblocks": agblocks,
        "guest_bytes": disk.stat().st_size,
        "guest_sha256": disk_hash,
        "xz_sha256": hashlib.sha256(dest.read_bytes()).hexdigest(),
        "files": vm,
        "xfs_info": info,
        "xfs_db_free_extents": extents,
        "journal_report": cmd("xfs_logprint", "-t", disk).splitlines()[-40:],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--fragmented", action="store_true")
    if parser.parse_args().fragmented:
        with tempfile.TemporaryDirectory() as tmp:
            capture = generate(Path(tmp), 4096, 4096, True, 3, True)
            assert len(capture["xfs_db_free_extents"]) > 1000
            (ROOT / "fragmented.json").write_text(
                json.dumps({"xfs-4096-4096-3": capture}, indent=2) + "\n"
            )
        raise SystemExit(0)

    manifest = {}
    for block, sector, reflink, ags in [
        (4096, 512, False, 4),
        (1024, 512, False, 3),
        (4096, 4096, True, 4),
    ]:
        with tempfile.TemporaryDirectory() as tmp:
            key = f"xfs-{block}-{sector}-{ags}"
            print("Capturing", key, flush=True)
            manifest[key] = generate(Path(tmp), block, sector, reflink, ags)
    (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
