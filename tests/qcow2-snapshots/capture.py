"""Generate internal snapshot disks from independently captured filesystem generations."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

TESTS = Path(__file__).resolve().parents[1]
CASES = {
    "v2-ntfs": ("vhd-chain/dynamic-flushed", "compat=0.10,cluster_size=4096", False),
    "v3-fat32": ("vdi-chain/fat32-zero", "compat=1.1,cluster_size=65536", False),
    "v3-ext4": ("vdi-chain/ext4-depth8", "compat=1.1,cluster_size=65536", False),
    "extended-ext4": ("vdi-chain/ext4-depth8", "compat=1.1,cluster_size=131072,extended_l2=on", False),
    "compressed-ntfs": ("vhd-chain/dynamic-flushed", "compat=1.1,cluster_size=65536,compression_type=zstd", True),
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def capture(label, reference, options, compressed, destination, qemu, io, work):
    source = TESTS / reference
    if reference.startswith("vhd-chain/"):
        history = json.loads((source / "manifest.json").read_text())
        inputs = [source / (m["filename"] + ".guest.raw.xz") for m in history["members"]]
    else:
        history = json.loads((source.parent / "manifest.json").read_text())[source.name]
        inputs = sorted(source.glob("*.raw.xz"))

    def run(*args):
        return subprocess.check_output(args, cwd=work, stderr=subprocess.STDOUT)

    snapshots = []
    disk = work / "disk.qcow2"
    previous = b""
    for i, path in enumerate(inputs):
        guest = lzma.decompress(path.read_bytes())
        (work / "input.raw").write_bytes(guest)
        if i == 0:
            run(qemu, "convert", "-f", "raw", "-O", "qcow2", "-o", options,
                *(["-c"] if compressed else []), "input.raw", disk)
        else:
            writes = []
            for offset in range(0, len(guest), 65536):
                data = guest[offset:offset + 65536]
                if data == previous[offset:offset + len(data)]:
                    continue
                if any(data):
                    sample = f"write-{offset}"
                    (work / sample).write_bytes(data)
                    writes += ["-c", f"write -s {sample} {offset} {len(data)}"]
                else:
                    writes += ["-c", f"write -z {offset} {len(data)}"]
            run(io, "-f", "qcow2", *writes, disk)
        name = ["before unlink", "削除後", "after removal"][i] if i < 3 else f"deleted {i}"
        run(qemu, "snapshot", "-c", name, disk)
        run(qemu, "compare", "-f", "raw", "-F", "qcow2", "input.raw", disk)
        snapshots.append(dict(name=name, reference=path.relative_to(TESTS).as_posix(),
                              guest_bytes=len(guest), guest_sha256=sha(guest)))
        previous = guest
    run(io, "-f", "qcow2", "-c", f"write -P 0xda {len(previous) - 512} 512", disk)
    active = previous[:-512] + bytes([0xda]) * 512
    run(qemu, "convert", "-f", "qcow2", "-O", "raw", disk, "active.raw")
    assert (work / "active.raw").read_bytes() == active
    info = json.loads(run(qemu, "info", "--output=json", disk))["snapshots"]
    assert len(info) == len(snapshots)
    for snapshot in snapshots:
        snapshot["id"] = next(s["id"] for s in info if s["name"] == snapshot["name"])
        for key in ("id", "name"):
            run(qemu, "convert", "-f", "qcow2", "-l", f"snapshot.{key}={snapshot[key]}",
                "-O", "raw", disk, "snapshot.raw")
            assert (work / "snapshot.raw").read_bytes() == lzma.decompress((TESTS / snapshot["reference"]).read_bytes())
            (work / "snapshot.raw").unlink()
    run(qemu, "check", "-f", "qcow2", disk)
    encoded = disk.read_bytes()
    out = destination / label
    out.mkdir(exist_ok=False)
    (out / "disk.qcow2.xz").write_bytes(lzma.compress(encoded, preset=3))
    report = dict(qemu=run(qemu, "--version").decode().splitlines()[0], options=options,
                  originals=history["originals"], image_sha256=sha(encoded), outer_bytes=len(encoded),
                  active_sha256=sha(active), snapshots=snapshots)
    (out / "manifest.json").write_text(json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
                                       encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--qemu-img", default="qemu-img")
    parser.add_argument("--qemu-io", default="qemu-io")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for case, config in CASES.items():
        with tempfile.TemporaryDirectory() as tmp:
            capture(case, *config, args.output, args.qemu_img, args.qemu_io, Path(tmp))
