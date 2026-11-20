"""Grow a QCOW2 twice, keeping the original NTFS snapshots across both changes."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
TESTS = HERE.parent


def capture(output, qemu, qemu_io, temp):
    source = HERE / "v2-ntfs"
    meta = json.loads((source / "manifest.json").read_text(encoding="utf-8"))
    old = meta["snapshots"][-1]
    original = lzma.decompress((TESTS / old["reference"]).read_bytes())
    current = original[:-512] + bytes([0xda]) * 512
    disk = temp / "resized.qcow2"
    with lzma.open(source / "disk.qcow2.xz") as src, disk.open("xb") as dst:
        shutil.copyfileobj(src, dst)
    subprocess.run([qemu, "amend", "-f", "qcow2", "-o", "compat=1.1", disk], check=True)
    output.mkdir(exist_ok=False)
    for index, marker in enumerate([0x57, 0x8e], 1):
        previous_length = len(current)
        added = 8 << 20
        subprocess.run([qemu, "resize", "-f", "qcow2", disk, f"+{added}"], check=True)
        # The new last sector is in an L1 entry which the old disk did not cover.
        current += bytes(added - 512) + bytes([marker]) * 512
        subprocess.run([qemu_io, "-f", "qcow2", "-c",
                        f"write -P {marker} {previous_length + added - 512} 512", disk], check=True)
        name = f"grown {index}"
        subprocess.run([qemu, "snapshot", "-c", name, disk], check=True)
        snaps = json.loads(subprocess.check_output([qemu, "info", "--output=json", disk]))["snapshots"]
        saved = next(s for s in snaps if s["name"] == name)
        filename = f"grown-{index}.raw.xz"
        (output / filename).write_bytes(lzma.compress(current, preset=3))
        meta["snapshots"].append(dict(name=name, id=saved["id"], guest_bytes=len(current),
            guest_sha256=hashlib.sha256(current).hexdigest(), reference=f"qcow2-snapshots/resized-ntfs/{filename}"))
    subprocess.run([qemu_io, "-f", "qcow2", "-c", f"write -P 0xda {len(current) - 512} 512", disk], check=True)
    active = current[:-512] + bytes([0xda]) * 512
    raw = temp / "check.raw"
    subprocess.run([qemu, "convert", "-O", "raw", disk, raw], check=True)
    assert raw.read_bytes() == active
    for snap in meta["snapshots"]:
        raw.unlink()
        subprocess.run([qemu, "convert", "-l", f"snapshot.id={snap['id']}", "-O", "raw", disk, raw], check=True)
        expected = lzma.decompress(((output / Path(snap["reference"]).name)
            if snap["reference"].startswith("qcow2-snapshots/") else TESTS / snap["reference"]).read_bytes())
        with raw.open("rb") as f:
            assert f.read(len(expected)) == expected
            tail = f.read()
            assert len(tail) in (0, len(active) - len(expected)) and not any(tail)
    subprocess.run([qemu, "check", "-f", "qcow2", disk], check=True)
    encoded = disk.read_bytes()
    meta.update(options="compat=1.1 (amended from v2),cluster_size=4096", qemu=subprocess.check_output([qemu, "--version"], text=True).splitlines()[0],
                active_sha256=hashlib.sha256(active).hexdigest(), image_sha256=hashlib.sha256(encoded).hexdigest(),
                outer_bytes=len(encoded))
    (output / "disk.qcow2.xz").write_bytes(lzma.compress(encoded, preset=3))
    (output / "manifest.json").write_text(json.dumps(meta, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
                                         encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--qemu-img", default="qemu-img")
    parser.add_argument("--qemu-io", default="qemu-io")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as working:
        capture(args.output, args.qemu_img, args.qemu_io, Path(working))
