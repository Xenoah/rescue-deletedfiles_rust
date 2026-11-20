"""Flatten explicitly selected QCOW2 chains and recover the genuinely unlinked guest files."""

import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check
from e2e_android_sparse import carve, originals
from e2e_html import make_deleted_fat
from e2e_netpbm import make_deleted_ext4
from e2e_vhdx import GOLD, CORPUS, digest, native_import, recover

REF = GOLD.parent / "qcow2-backing"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def cmd(folder, *args):
    return subprocess.run(
        list(map(str, args)), cwd=folder, check=True, capture_output=True, timeout=180
    ).stdout


def write_child(root, child, backing, backing_format, options, guest, ranges, zeros):
    cmd(
        root,
        "qemu-img",
        "create",
        "-f",
        "qcow2",
        "-o",
        options,
        "-F",
        backing_format,
        "-b",
        backing.name,
        child.name,
        str(len(guest)),
    )
    for start, end in ranges:
        (root / "samples.bin").write_bytes(guest[start:end])
        cmd(
            root,
            "qemu-io",
            "-f",
            "qcow2",
            "-c",
            f"write -s samples.bin {start} {end - start}",
            child.name,
        )
    for start, end in zeros:
        check(not any(guest[start:end]), start)
        cmd(root, "qemu-io", "-f", "qcow2", "-c", f"write -z {start} {end - start}", child.name)
    cmd(root, "qemu-img", "check", child.name)


def create(root, guest, files, profile):
    # Introduce distinct older data at two deleted files and in an unused range.
    # Each younger layer supplies just its changes. The rest must come from its parent.
    positions = []
    for name in dict.fromkeys(["sample.png", "sample.wav", *files]):
        p = guest.find(files[name][: min(len(files[name]), 32)]) if name in files else -1
        if p >= 0 and p // 512 * 512 not in positions:
            positions.append(p // 512 * 512)
    check(len(positions) >= 2, positions)
    a, b = positions[:2]
    hole = guest.find(bytes(131072), 1 << 20)
    check(hole >= 0, hole)
    hole = (hole + 65535) // 65536 * 65536
    old = bytearray(guest)
    old[a : a + 512] = b"A" * 512
    old[b : b + 512] = b"B" * 512
    old[hole : hole + 65536] = b"Q" * 65536
    parent = root / "base.raw"
    if profile == "short":
        # A shorter parent supplies zeros beyond its logical end.
        check(not any(old[-65536:]), "short parent requires a zero tail")
        del old[-65536:]
    parent.write_bytes(old)
    backing = parent
    parent_kind = "raw"
    if profile in ("deflate", "zstd"):
        backing = root / "base.qcow2"
        options = "cluster_size=65536"
        if profile == "zstd":
            options += ",compression_type=zstd"
        cmd(
            root,
            "qemu-img",
            "convert",
            "-f",
            "raw",
            "-O",
            "qcow2",
            "-c",
            "-o",
            options,
            parent.name,
            backing.name,
        )
        parent_kind = "qcow2"
        check(
            json.loads(cmd(root, "qemu-img", "check", "--output=json", backing.name))[
                "compressed-clusters"
            ]
            > 0,
            backing,
        )
    chain = [backing]
    top_writes = [(a, a + 512), (b, b + 512)]
    top_zero = [(hole, hole + 65536)]
    if profile in ("nested", "deflate", "zstd"):
        middle = root / "middle.qcow2"
        write_child(
            root,
            middle,
            backing,
            parent_kind,
            "cluster_size=4096",
            guest,
            [top_writes.pop()],
            [top_zero.pop()],
        )
        chain.insert(0, middle)
        parent_kind = "qcow2"
    child = root / "child.qcow2"
    config = {
        "v2": "compat=0.10,cluster_size=4096",
        "v3": "cluster_size=4096",
        "short": "cluster_size=4096",
    }.get(profile, "cluster_size=16384,extended_l2=on")
    write_child(root, child, chain[0], parent_kind, config, guest, top_writes, top_zero)
    chain.insert(0, child)
    spans = json.loads(cmd(root, "qemu-img", "map", "--output=json", child.name))
    depths = sorted({s["depth"] for s in spans})
    check(depths == list(range(len(chain))), (profile, depths, len(chain)))
    qemu = root / "qemu.raw"
    cmd(root, "qemu-img", "convert", "-f", "qcow2", "-O", "raw", child.name, qemu.name)
    check(qemu.read_bytes() == guest and digest(qemu) == sha(guest), profile)
    qemu.unlink()
    return chain, depths


def checked_import(binary, chain, out, options, code=0, kind="qcow2"):
    args = [
        binary,
        "import-image",
        "--source",
        chain[0],
        "--image-type",
        kind,
        "--output",
        out,
        "--json",
    ]
    for member in chain[1:]:
        args.extend(["--segment", member])
    proc = subprocess.run(
        list(map(str, [*args, *options])),
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=dict(os.environ, PATH=""),
        timeout=180,
    )
    check(proc.returncode == code, (args, proc.stdout, proc.stderr))
    if code:
        check(not out.exists(), out)
        return None
    return json.loads(proc.stdout)


def unrelated_base(folder, destination, size):
    disk = folder / "unrelated.qcow2"
    cmd(folder, "qemu-img", "create", "-f", "qcow2", disk.name, str(size))
    cmd(folder, "qemu-io", "-f", "qcow2", "-c", "write -P 0xbc 0 4096", disk.name)
    cmd(folder, "qemu-img", "check", disk.name)
    raw = disk.read_bytes()
    (destination / "unrelated.qcow2.xz").write_bytes(lzma.compress(raw))
    return sha(raw)


def restore(binary, root, chain, guest, files, windows=None):
    before = [digest(f) for f in chain]
    out = root / "restored.raw"
    copies = {}
    for label, kind, indexed in [("ordinary", None, False), ("indexed", "qcow2", True)]:
        report = (
            checked_import(
                binary, chain, out, ["--raw-sha256", sha(guest).upper(), "--raw-bytes", len(guest)]
            )
            if indexed
            else native_import(binary, chain[0], out, kind, segments=chain[1:])
        )
        check(
            report["bytes"] == len(guest)
            and out.read_bytes() == guest
            and report["sha256"] == sha(guest),
            report,
        )
        rows = (
            recover(binary, out, root / label, windows, indexed)
            if windows
            else carve(binary, out, root / label, files, False, indexed)
        )
        copies[label] = rows
        out.unlink()
    check(copies["ordinary"] == copies["indexed"] and [digest(f) for f in chain] == before, root)
    print(
        root.name,
        len(chain),
        "chain members;",
        len(copies["ordinary"]),
        "unlinked files byte-exact in both scan modes",
        flush=True,
    )
    return {"guest_sha256": sha(guest), "guest_bytes": len(guest), "originals": copies["ordinary"]}


def native(binary, root, fixture=None):
    files = originals()
    result = {}
    for fs, profiles in [
        ("fat32", ["v2", "v3", "short", "nested", "zstd"]),
        ("ext4", ["extended", "deflate"]),
        ("windows-ntfs", ["zstd"]),
    ]:
        fsroot = root / fs
        fsroot.mkdir()
        windows = None
        samples = files
        if fs == "windows-ntfs":
            windows = json.loads((GOLD / "fixed/manifest.json").read_text())
            vhd = fsroot / "windows.vhdx"
            vhd.write_bytes(lzma.decompress((GOLD / "fixed/volume.vhdx.xz").read_bytes()))
            raw = fsroot / "windows.raw"
            cmd(fsroot, "qemu-img", "convert", "-f", "vhdx", "-O", "raw", vhd.name, raw.name)
            check(digest(raw) == windows["guest_sha256"], windows)
            samples = {n: (CORPUS / n).read_bytes() for n in windows["originals"]}
            vhd.unlink()
        else:
            raw = (make_deleted_fat if fs == "fat32" else make_deleted_ext4)(fsroot, samples)
        guest = raw.read_bytes()
        for profile in profiles:
            name = fs + "-" + profile
            case = fsroot / name
            case.mkdir()
            chain, depths = create(case, guest, samples, profile)
            proof = restore(binary, case, chain, guest, samples, windows)
            proof["qemu_depths"] = depths
            result[name] = proof
            if fixture and name == "fat32-zstd":
                fixture.mkdir(exist_ok=True, parents=True)
                meta = {
                    "producer": cmd(case, "qemu-img", "--version").decode().splitlines()[0],
                    "members": {},
                    "chain": [p.name for p in chain],
                    "expected": proof,
                }
                for p in chain:
                    data = p.read_bytes()
                    (fixture / (p.name + ".xz")).write_bytes(lzma.compress(data))
                    meta["members"][p.name] = sha(data)
                meta["unrelated_qcow2_sha256"] = unrelated_base(case, fixture, len(guest))
                (fixture / "expected.raw.xz").write_bytes(lzma.compress(guest))
                (fixture / "manifest.json").write_text(json.dumps(meta, indent=2) + "\n")
            for p in case.glob("*.raw"):
                p.unlink()
    return result


def invalid_chains(binary, root, chain, guest):
    child, middle, base = chain
    original = child.read_bytes()
    old_middle = middle.read_bytes()
    parent_at, parent_len = struct.unpack_from(">QI", original, 8)
    fmt = struct.unpack_from(">I", original, 100)[0]
    check(
        original[fmt : fmt + 4] == bytes.fromhex("e2792aca")
        and original[fmt + 8 : fmt + 13] == b"qcow2",
        "missing image-format extension",
    )
    region = root / "negative"
    region.mkdir()
    out = region / "should-not-exist.raw"
    refused = []

    def refuse(label, source, parents, code):
        native_import(binary, source, out, "qcow2", code, parents)
        refused.append(label)

    refuse("missing-all-parents", child, [], 4)
    refuse("missing-base", child, [middle], 2)
    refuse("wrong-order", child, [base, middle], 4)
    refuse("extra-parent", middle, [base, child], 4)
    refuse("repeated-path", child, [middle, middle], 2)
    alien = region / "unrelated.qcow2"
    alien.write_bytes(old_middle)
    refuse("parent-name-mismatch", child, [alien, base], 4)

    def altered(offset, replacement):
        return original[:offset] + replacement + original[offset + len(replacement) :]

    bad = region / "overlay.qcow2"
    cases = {
        "backing-offset-zero": altered(8, bytes(8)),
        "backing-length-zero": altered(16, bytes(4)),
        "huge-name": altered(16, (1024).to_bytes(4, "big")),
        "name-outside-header": altered(8, (1 << 40).to_bytes(8, "big")),
        "name-inside-extensions": altered(8, fmt.to_bytes(8, "big")),
        "no-format-extension": altered(fmt, b"????"),
        "oversize-extension": altered(fmt + 4, bytes([255]) * 4),
        "unsupported-parent-format": altered(fmt + 8, b"vmdk "),
        "raw-format-with-extra-parent": altered(fmt + 4, struct.pack(">I", 3) + b"raw" + bytes(5)),
        "remote-name": altered(parent_at, b"nbd:" + b"x" * (parent_len - 4)),
        "control-in-name": altered(parent_at, b"\0"),
        "invalid-name-encoding": altered(parent_at, b"\xff"),
        "encrypted-child": altered(32, struct.pack(">I", 1)),
    }
    for label, data in cases.items():
        bad.write_bytes(data)
        refuse(label, bad, [middle, base], 4)
    # A relocated chain is explicitly selected and the stored directory is never opened.
    new_name = b"../archived/snapshots/" + middle.name.encode()
    renamed = bytearray(original)
    struct.pack_into(">I", renamed, 16, len(new_name))
    renamed[parent_at : parent_at + len(new_name)] = new_name
    bad.write_bytes(renamed)
    native_import(binary, bad, out, "qcow2", segments=[middle, base])
    check(out.read_bytes() == guest, "relocated parent")
    out.unlink()
    # Match an on-disk name but give it a different container format or missing data.
    broken = region / base.name
    broken.write_bytes(b"not a QCOW2 disk" * 16)
    refuse("invalid-base-header", child, [middle, broken], 4)
    broken.write_bytes(base.read_bytes()[:104])
    refuse("truncated-compressed-base", child, [middle, broken], 4)
    bad.write_bytes(original[: len(original) - 1])
    refuse("truncated-allocated-subcluster", bad, [middle, base], 4)
    for label, options, code in [
        ("original-length", ["--raw-bytes", len(guest) + 512], 4),
        ("original-digest", ["--raw-sha256", "00" * 32], 4),
        ("malformed-original-digest", ["--raw-sha256", "abc"], 2),
    ]:
        checked_import(binary, chain, out, options, code)
        refused.append(label)
    # A valid same-name substitute is structurally indistinguishable without a known guest hash.
    altered_base = lzma.decompress((REF / "unrelated.qcow2.xz").read_bytes())
    check(
        sha(altered_base)
        == json.loads((REF / "manifest.json").read_text())["unrelated_qcow2_sha256"],
        broken,
    )
    broken.write_bytes(altered_base)
    substitute = [child, middle, broken]
    without_reference = native_import(binary, child, out, "qcow2", segments=substitute[1:])
    check(
        without_reference["sha256"] == digest(out) != sha(guest),
        "same-name substitute must change disk bytes",
    )
    out.unlink()
    checked_import(binary, substitute, out, ["--raw-sha256", sha(guest)], 4)
    refused.append("same-name-valid-substitute")
    # Existing output is protected even after a complete chain has been supplied.
    out.write_bytes(b"preserve")
    args = [
        binary,
        "import-image",
        "--source",
        child,
        "--segment",
        middle,
        "--segment",
        base,
        "--output",
        out,
        "--json",
    ]
    proc = subprocess.run(list(map(str, args)), env=dict(os.environ, PATH=""), capture_output=True)
    check(proc.returncode and out.read_bytes() == b"preserve", "existing output changed")
    return sorted(refused)


def standalone(binary, root):
    result = {}
    for kind, folder, stored, length_key, hash_key in [
        ("qcow1", "qcow-v1", "fat32.qcow.xz", "raw_bytes", "raw_sha256"),
        ("qcow2", "qcow2-subclusters", "fat32.qcow2.xz", "guest_bytes", "guest_sha256"),
    ]:
        reference = REF.parent / folder
        meta = json.loads((reference / "manifest.json").read_text())
        image = root / ("single." + kind)
        image.write_bytes(lzma.decompress((reference / stored).read_bytes()))
        out = root / (kind + ".raw")
        verified = checked_import(
            binary,
            [image],
            out,
            ["--raw-bytes", meta[length_key], "--raw-sha256", meta[hash_key]],
            kind=kind,
        )
        check(digest(out) == meta[hash_key] == verified["sha256"], verified)
        result[kind] = carve(binary, out, root / ("single-" + kind), originals(), False)
        out.unlink()
        checked_import(binary, [image], out, ["--raw-sha256", "a5" * 32], 4, kind)
    return result


def portable(binary, root):
    meta = json.loads((REF / "manifest.json").read_text())
    chain = [root / n for n in meta["chain"]]
    for p in chain:
        p.write_bytes(lzma.decompress((REF / (p.name + ".xz")).read_bytes()))
        check(digest(p) == meta["members"][p.name], p)
    guest = lzma.decompress((REF / "expected.raw.xz").read_bytes())
    proof = restore(binary, root, chain, guest, originals())
    check(
        proof["guest_sha256"] == meta["expected"]["guest_sha256"]
        and proof["originals"] == meta["expected"]["originals"],
        proof,
    )
    return {
        **proof,
        "refused": invalid_chains(binary, root, chain, guest),
        "standalone": standalone(binary, root),
    }


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--native", action="store_true")
    p.add_argument("--write-fixture", type=Path)
    p.add_argument("--output", type=Path)
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        proof = (
            native(a.binary.resolve(), Path(scratch), a.write_fixture)
            if a.native
            else portable(a.binary.resolve(), Path(scratch))
        )
    if a.output:
        a.output.write_text(json.dumps(proof, indent=2, sort_keys=True) + "\n")
