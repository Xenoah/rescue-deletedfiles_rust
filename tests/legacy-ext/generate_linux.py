"""Capture ext2/ext3 on owned loopback images with the Linux filesystem driver."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
NAMES = tuple("sample." + ext for ext in ("bmp", "gif", "jpg", "pdf", "png", "sqlite", "wav", "webp", "zip"))


def command(*args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, capture_output=True, **kwargs)


def sha(value):
    return hashlib.sha256(value).hexdigest()


def record(image, output):
    disk = image.read_bytes()
    block = 1024 << int.from_bytes(disk[1048:1052], "little")
    files = {}
    for name in NAMES:
        content = (ROOT.parent / "corpus/files" / name).read_bytes()
        blockno = int(command("debugfs", "-R", "bmap /saved/" + name + " 0", image).stdout)
        positions = [match.start() for match in re.finditer(re.escape(content), disk)]
        live = blockno * block
        assert len(positions) == 2 and live in positions, (name, positions)
        deleted, = (position for position in positions if position != live)
        files[name] = dict(bytes=len(content), sha256=sha(content), live_offset=live, deleted_offset=deleted, deleted_bytes_retained=True)
    command("e2fsck", "-f", "-n", image)
    assert image.read_bytes() == disk
    (output / (image.name + ".xz")).write_bytes(lzma.compress(disk))
    return dict(disk_sha256=sha(disk), bytes=len(disk), block_bytes=block, originals=files)


def generate(root, output, kind, block, original_revision=False, indirect=False):
    disk = root / (f"{kind}-{block}" + ("-rev0" if original_revision else "-indirect" if indirect else "") + ".raw")
    with disk.open("xb") as f:
        f.truncate(24 << 20)
    options = ["-E", "nodiscard,lazy_itable_init=0,lazy_journal_init=0"]
    env = None
    if original_revision:
        config = root / "classic.conf"
        config.write_text("[defaults]\n blocksize = 1024\n inode_size = 128\n inode_ratio = 8192\n[fs_types]\n ext2 = {\n }\n")
        env = dict(os.environ, MKE2FS_CONFIG=str(config))
        options = ["-I", "128", "-E", "revision=0,nodiscard,lazy_itable_init=0,lazy_journal_init=0"]
    command("mke2fs", "-q", "-F", "-t", kind, "-b", block, "-g", 1024, *options, disk, env=env)
    mount = root / "volume"
    mount.mkdir(exist_ok=True)
    command("mount", "-t", "ext4", "-o", "loop,nodiscard", disk, mount)
    try:
        for folder in ("deleted", "saved"):
            (mount / folder).mkdir()
            for name in NAMES:
                contents = (ROOT.parent / "corpus/files" / name).read_bytes()
                path = mount / folder / name
                path.write_bytes(contents)
                assert path.read_bytes() == contents
            command("sync")
        if indirect:
            sparse = mount / "saved/sparse-live.bin"
            with sparse.open("wb") as f:
                for number in (2, 18, 700, 70000):
                    f.seek(number * 1024)
                    f.write(b"Native sparse file, mapped beyond triple-indirect threshold.\n")
            os.link(sparse, mount / "saved/hardlink")
            (mount / "saved/short-link").symlink_to("sparse-live.bin")
            (mount / "saved/long-link").symlink_to("/mnt/saved/a-long-symbolic-link-target-that-needs-a-data-block-to-hold-the-entire-name-in-ext3")
            command("sync")
        for name in NAMES:
            (mount / "deleted" / name).unlink()
        assert not list((mount / "deleted").iterdir())
    finally:
        command("umount", mount)
    return disk.stem, record(disk, output)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("output", type=Path)
    args = p.parse_args()
    args.output.mkdir(exist_ok=True, parents=True)
    with tempfile.TemporaryDirectory() as tmp:
        results = dict(generate(Path(tmp), args.output, kind, block) for kind in ("ext2", "ext3") for block in (1024,2048,4096))
        name, result = generate(Path(tmp), args.output, "ext2", 1024, True)
        results[name] = result
        name, result = generate(Path(tmp), args.output, "ext3", 1024, indirect=True)
        results[name] = result
    (args.output / "manifest.json").write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
