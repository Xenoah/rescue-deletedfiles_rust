"""Compare QCOW2 Zstandard and extended L2 imports to QEMU and actual filesystem deletions."""

import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
from e2e import check
from e2e_android_sparse import originals, carve
from e2e_extended_filesystems import make_deleted
from e2e_html import make_deleted_fat
from e2e_vhdx import GOLD, digest, recover

REF = GOLD.parent / "qcow2-subclusters"
OFFSET = 0x00FFFFFFFFFFFE00
COMPRESSED = 1 << 62


def command(*args):
    return subprocess.run(
        list(map(str, args)), capture_output=True, check=True, timeout=180
    ).stdout


def native_import(binary, source, out, expected=0):
    p = subprocess.run(
        [binary, "import-image", "--source", source, "--output", out, "--json"],
        env=dict(os.environ, PATH=""),
        capture_output=True,
        encoding="utf-8",
        timeout=180,
    )
    check(p.returncode == expected, (p.returncode, p.stdout, p.stderr))
    if expected:
        check(not out.exists(), out)
        return None
    return json.loads(p.stdout)


def entry(image, logical):
    cluster = 1 << struct.unpack_from(">I", image, 20)[0]
    per_l2 = cluster // 16
    l1 = struct.unpack_from(">Q", image, 40)[0]
    table = (
        struct.unpack_from(">Q", image, l1 + 8 * (logical // cluster // per_l2))[0]
        & OFFSET
    )
    if not table:
        return 0, 0, 0
    pos = table + 16 * ((logical // cluster) % per_l2)
    desc, bits = struct.unpack_from(">QQ", image, pos)
    return pos, desc, bits


def create_capture(raw, case, cluster, extended, codec):
    disk = raw.read_bytes()
    image = case / "disk.qcow2"
    options = f"cluster_size={cluster},compression_type={codec},extended_l2={'on' if extended else 'off'}"
    command(
        "qemu-img",
        "convert",
        "-f",
        "raw",
        "-O",
        "qcow2",
        "-c",
        "-o",
        options,
        raw,
        image,
    )
    expected = disk
    if extended:
        check(len(disk) % cluster == 0, "fixture geometry")
        # Keep the original filesystem intact; use a new cluster beyond its end.
        command("qemu-img", "resize", "-f", "qcow2", image, f"+{cluster}")
        sub = cluster // 32
        command(
            "qemu-io",
            "-f",
            "qcow2",
            "-c",
            f"write -P 0x71 {len(disk)} {sub}",
            "-c",
            f"write -z {len(disk)+sub} {sub}",
            "-c",
            f"write -P 0x37 {len(disk)+31*sub} {sub}",
            image,
        )
        trailer = b"q" * sub + bytes(30 * sub) + b"7" * sub
        expected = disk + trailer
        _, desc, bits = entry(image.read_bytes(), len(disk))
        check(bits == 0x280000001 and desc & OFFSET, hex(bits))
        # Raw host storage can retain data behind both an explicit zero and an unallocated subcluster.
        with image.open("r+b") as f:
            f.seek((desc & OFFSET) + sub)
            f.write(bytes([0xA9]) * (2 * sub))
    report = json.loads(command("qemu-img", "check", "--output=json", image))
    check(
        report["check-errors"] == 0 and report.get("compressed-clusters", 0) > 0, report
    )
    baseline = case / "qemu.raw"
    command("qemu-img", "convert", "-f", "qcow2", "-O", "raw", image, baseline)
    check(
        baseline.read_bytes() == expected,
        "independent QEMU guest including stale host bytes",
    )
    return image, expected, report["compressed-clusters"]


def extract(binary, image, expected, folder, files, windows=None):
    out = folder / "native.raw"
    before = digest(image)
    report = native_import(binary, image, out)
    check(
        out.read_bytes() == expected
        and report["bytes"] == len(expected)
        and report["sha256"] == hashlib.sha256(expected).hexdigest()
        and digest(image) == before,
        report,
    )
    rows = (
        recover(binary, out, folder / "restored", windows)
        if windows
        else carve(binary, out, folder / "restored", files, False)
    )
    return dict(
        disk_bytes=len(expected),
        disk_sha256=report["sha256"],
        image_sha256=before,
        originals=rows,
    )


def write_reference(stage, destination):
    files = originals()
    raw = make_deleted_fat(stage, files)
    image, expected, count = create_capture(raw, stage, 16384, True, "zstd")
    stored = image.read_bytes()
    destination.mkdir(parents=True, exist_ok=True)
    packed = lzma.compress(stored)
    (destination / "fat32.qcow2.xz").write_bytes(packed)
    # A checksummed frame from the independent zstd CLI exercises length and checksum verification.
    data = stage / "cluster"
    data.write_bytes(bytes([0x51]) * 16384)
    frame = command("zstd", "--check", "-q", "-c", data)
    (destination / "cluster.zst").write_bytes(frame)
    manifest = dict(
        producer=command("qemu-img", "--version").decode().splitlines()[0],
        guest_sha256=hashlib.sha256(expected).hexdigest(),
        guest_bytes=len(expected),
        packed_sha256=hashlib.sha256(packed).hexdigest(),
        container_sha256=digest(image),
        frame_sha256=hashlib.sha256(frame).hexdigest(),
        filesystem_bytes=raw.stat().st_size,
        compressed_clusters=count,
        originals={n: hashlib.sha256(d).hexdigest() for n, d in files.items()},
    )
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


def portable(binary, root):
    meta = json.loads((REF / "manifest.json").read_text())
    packed = (REF / "fat32.qcow2.xz").read_bytes()
    stored = lzma.decompress(packed)
    check(hashlib.sha256(packed).hexdigest() == meta["packed_sha256"], meta)
    image = root / "frozen.qcow2"
    image.write_bytes(stored)
    out = root / "frozen.raw"
    report = native_import(binary, image, out)
    check(
        digest(image) == meta["container_sha256"]
        and digest(out) == report["sha256"] == meta["guest_sha256"]
        and out.stat().st_size == meta["guest_bytes"],
        report,
    )
    files = carve(binary, out, root / "checked", originals(), False)
    check(files == meta["originals"], files)
    cluster = 16384
    at, desc, bitmap = entry(stored, meta["filesystem_bytes"])
    check(bitmap == 0x280000001 and desc & OFFSET, "expected mixed last allocation")
    compressed = [
        entry(stored, guest)
        for guest in range(0, meta["filesystem_bytes"], cluster)
        if entry(stored, guest)[1] & COMPRESSED
    ]
    check(len(compressed) == meta["compressed_clusters"], compressed)
    first, packed_desc, _ = compressed[0]
    size_bits = cluster.bit_length() - 1 - 8
    packed_at = packed_desc & ((1 << (62 - size_bits)) - 1)
    frame = (REF / "cluster.zst").read_bytes()
    check(hashlib.sha256(frame).hexdigest() == meta["frame_sha256"], "zstd reference")

    def modified(offset, data):
        body = bytearray(stored)
        body[offset : offset + len(data)] = data
        return body

    def word(v):
        return struct.pack(">Q", v)

    check(first == entry(stored, 0)[0], "first guest cluster must be compressed")
    checksummed = root / "checksummed.qcow2"
    checksummed.write_bytes(modified(packed_at, frame))
    checksummed_raw = root / "checksummed.raw"
    native_import(binary, checksummed, checksummed_raw)
    check(
        checksummed_raw.read_bytes() == b"Q" * cluster + out.read_bytes()[cluster:],
        "valid independent Zstandard frame did not replace exactly one cluster",
    )
    oversize = cluster + 1
    long_frame = (
        bytes.fromhex("28b52ffd60")
        + struct.pack("<H", oversize - 256)
        + ((oversize << 3) | 3).to_bytes(3, "little")
        + b"X"
    )
    cases = {
        "long-zstd-output": modified(packed_at, long_frame),
        "missing-compression-field": modified(100, struct.pack(">I", 104)),
        "codec-zero": modified(104, b"\x00"),
        "unknown-codec": modified(104, b"\x02"),
        "codec-without-bit": modified(72, word(16)),
        "unknown-flag": modified(72, word(56)),
        "small-subcluster": modified(20, struct.pack(">I", 13)),
        "thin-l1": modified(36, struct.pack(">I", 1)),
        "reserved-cluster-zero": modified(at, word(desc | 1)),
        "conflicting-bits": modified(at + 8, word(bitmap | 2)),
        "no-host": modified(at, word(0)),
        "compressed-bitmap": modified(first + 8, word(1)),
        "compressed-magic": modified(packed_at, b"oops"),
        "invalid-zstd-checksum": modified(
            packed_at, frame[:-1] + bytes([frame[-1] ^ 1])
        ),
        "short-zstd-output": modified(packed_at, bytes.fromhex("28b52ffd20010b000041")),
    }
    for name, value in cases.items():
        bad = root / (name + ".qcow2")
        bad.write_bytes(value)
        native_import(binary, bad, root / (name + ".raw"), 4)
    print(
        "QEMU reference: seven real FAT32 deletions, mixed subclusters, stale host bytes and",
        len(cases),
        "malformed cases passed",
        flush=True,
    )
    return files


def native(binary, root):
    rows = {}
    files = originals()
    for fs, cluster, extended, codec in [
        ("fat32", 16384, True, "zstd"),
        ("ext4", 65536, True, "zlib"),
        ("windows-ntfs", 65536, False, "zstd"),
    ]:
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            stage = Path(tmp)
            manifest = None
            if fs == "fat32":
                raw = make_deleted_fat(stage, files)
            elif fs == "ext4":
                for name, data in files.items():
                    (stage / name).write_bytes(data)
                raw = make_deleted(stage, "ext4", files)
            else:
                manifest = json.loads((GOLD / "fixed/manifest.json").read_text())
                disk = stage / "win.vhdx"
                raw = stage / "windows.raw"
                with lzma.open(GOLD / "fixed/volume.vhdx.xz") as src, disk.open(
                    "wb"
                ) as dst:
                    shutil.copyfileobj(src, dst)
                command("qemu-img", "convert", "-f", "vhdx", "-O", "raw", disk, raw)
                check(digest(raw) == manifest["guest_sha256"], "Windows guest hash")
            image, expected, count = create_capture(
                raw, stage, cluster, extended, codec
            )
            row = extract(binary, image, expected, stage, files, manifest)
            row.update(
                cluster_size=cluster,
                compression=codec,
                extended_l2=extended,
                compressed_clusters=count,
            )
            rows[fs] = row
            print(
                fs,
                codec,
                extended,
                count,
                "compressed clusters and seven genuine deletions verified",
                flush=True,
            )
    return rows


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path)
    p.add_argument("--native", action="store_true")
    p.add_argument("--evidence", type=Path)
    p.add_argument("--write-reference", type=Path)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        if args.write_reference:
            write_reference(Path(tmp), args.write_reference)
            raise SystemExit(0)
        if not args.binary:
            p.error("--binary is required")
        rows = (native if args.native else portable)(args.binary.resolve(), Path(tmp))
    if args.evidence:
        args.evidence.write_text(json.dumps(rows, indent=2) + "\n")
