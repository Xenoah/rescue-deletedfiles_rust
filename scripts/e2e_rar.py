"""Recover real RAR 4/5 archives from adjacent storage and deleted FAT32/ext4 files."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

from archive_trials import digest, recover_archives
from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / "tests/rar"
CORPUS = GOLD.parent / "corpus/files"


def decoded_members(unrar, path, members):
    subprocess.run([unrar, "t", "-inul", "-p-", path], check=True)
    for name, sha in members.items():
        data = subprocess.check_output([unrar, "p", "-inul", "-p-", path, name])
        check(data == (b"" if Path(name).name == "empty.bin" else (CORPUS / Path(name).name).read_bytes()) and digest(data) == sha, (path, name))


def rejected(binary, root, files):
    negatives = {p.stem: p.read_bytes() for p in GOLD.glob("*.unsupported")}
    for generation in (4, 5):
        clean = files[f"rar{generation}-store.rar"]
        first = 7 if generation == 4 else 8
        for label, pos in (("main-crc", first), ("member-body", clean.index((CORPUS / "sample.png").read_bytes()) + 20),
                           ("end-crc", len(clean) - (7 if generation == 4 else 8))):
            damaged = bytearray(clean)
            damaged[pos] ^= 0x40
            negatives[f"rar{generation}-{label}"] = damaged
        negatives[f"rar{generation}-truncated"] = clean[:-1]
        negatives[f"rar{generation}-missing-end"] = clean[:-10]
        compressed = files[f"rar{generation}-normal.rar"]
        # Damage the last part of a compressed member; all container headers keep their CRCs.
        changed = bytearray(compressed)
        changed[-27] ^= 0x81
        negatives[f"rar{generation}-compressed-data"] = changed
    clean = files["rar5-store.rar"]
    negatives["incomplete-size-varint"] = clean[:12] + b"\x80" * 3
    negatives["future-marker"] = clean[:6] + b"\x02" + clean[7:]
    updated = bytearray(clean)
    # The RAR5 end block uses an end-of-volume flag. Give this edit a valid CRC.
    updated[-1] = 1
    struct.pack_into("<I", updated, len(updated) - 8, zlib.crc32(updated[-4:]))
    negatives["next-volume-end"] = updated
    for v in (4, 5):
        src = files[f"rar{v}-comment-stored.rar"]
        if v == 4:
            start = src.index(b"CMT") + 3
        else:
            start = src.index(b"Stored images and an empty file.")
        changed = bytearray(src)
        changed[start + 3] ^= 0x20
        negatives[f"rar{v}-damaged-comment"] = changed
    src = files["rar5-blake.rar"]
    start = src.rindex(b"QO") + 2
    changed = bytearray(src)
    changed[start + 7] ^= 1
    negatives["damaged-quick-open-record-crc"] = changed
    # Keep the cached record's checksum valid while modifying a copied original header.
    changed = bytearray(src)
    end = start + 5 + changed[start + 4]  # This fixture has one record with a one-byte length.
    check(end == len(src) - 8, end)
    changed[end - 1] ^= 1
    struct.pack_into("<I", changed, start, zlib.crc32(changed[start + 4:end]))
    negatives["quick-open-copy-disagrees"] = changed
    for name, value in negatives.items():
        path = root / (name + ".raw")
        path.write_bytes(value)
        recognized = value.startswith((b"Rar!\x1a\x07\0", b"Rar!\x1a\x07\1\0"))
        _, rows = candidates(binary, path, root / (name + "-scan"), ["--extension", "rar"], expected=5 if recognized else 0)
        check(all(not row["supported"] for row in rows) and bool(rows) == recognized, (name, rows))
    gap = root / "missing.raw"
    saved = files["rar5-blake.rar"]
    at = ((len(saved) + 511) // 512 + 1) * 512
    gap.write_bytes(saved.ljust(at, b"\0") + files["rar4-normal.rar"] + bytes(2048))
    acquired = run(binary, "image", "--source", gap, "--block-size", "4096", "--destination", root / "acquisition", "--json")
    image = Path(acquired["image"])
    mark_gap(image, 4096)
    _, rows = candidates(binary, image, root / "missing-scan", ["--extension", "rar"], expected=5)
    check([row["record_sha256"] for row in rows if row["supported"]] == [digest(files["rar4-normal.rar"])], rows)
    return sorted([*negatives, "unread-member-range"])


def changed_comment(binary, root, original):
    # A newly computed comment CRC and header CRC describe a different valid archive.
    # The original archive's independently recorded SHA-256 must distinguish it.
    data = bytearray(original)
    comment = b"Stored images and an empty file.\r\n"
    at = data.index(comment)
    service = data.index(b"CMT") + 3
    check(at == service, at)
    payload_crc = data.index(struct.pack("<I", zlib.crc32(comment)))
    main_body = 13
    service_start = main_body + data[12]
    header_size = data[service_start + 4]
    check(service_start + 5 + header_size == at, at)
    data[at] ^= 0x20
    struct.pack_into("<I", data, payload_crc, zlib.crc32(data[at:at + len(comment)]))
    struct.pack_into("<I", data, service_start, zlib.crc32(data[service_start + 4:at]))
    path = root / "edited.rar"
    path.write_bytes(data)
    session, rows = candidates(binary, path, root / "edited-scan", ["--extension", "rar"])
    check(len(rows) == 1 and rows[0]["record_sha256"] == digest(data) != digest(original), rows)
    saved = run(binary, "recover", "--session", session, "--id", rows[0]["id"], "--destination", root / "edited-files", "--json")
    check(Path(saved["results"][0]["path"]).read_bytes() == data, saved)
    sums = root / "original.sha256"
    sums.write_text(digest(original) + "  original.rar\n", encoding="utf-8")
    match = run(binary, "match-hashes", "--report", saved["report"], "--checksums", sums, "--json")["comparison"]
    check(match["counts"] == {"matched": 0, "not_listed": 1, "not_comparable": 0}, match)
    return {"original": digest(original), "changed": digest(data), "match": "not_listed"}


def verify(binary, root, unrar=None):
    manifest = json.loads((GOLD / "manifest.json").read_text(encoding="utf-8"))
    files = {name: (GOLD / name).read_bytes() for name in manifest}
    for name, data in files.items():
        check(len(data) == manifest[name]["bytes"] and digest(data) == manifest[name]["sha256"], name)
    if unrar:
        disks = [("fat32", make_deleted_fat), ("ext4", lambda p, f: make_deleted_ext4(p, f, block_size=4096))]
    else:
        disks = [("adjacent", None)]
    proof = {}
    for label, create in disks:
        folder = root / label
        folder.mkdir()
        if create:
            disk = create(folder, files)
        else:
            disk = folder / "adjacent.raw"
            disk.write_bytes(b"\xe5" * ((1 << 20) - 3) + b"".join(files.values()) + bytes(1024))
        def inspect(path, name):
            decoded_members(unrar, path, manifest[name]["members"])
        proof[label] = recover_archives(binary, folder, disk, files, "rar", inspect if unrar else None)
    proof["refused"] = rejected(binary, root, files)
    proof["changed_comment"] = changed_comment(binary, root, files["rar5-comment-stored.rar"])
    if unrar:
        proof["members"] = {name: info["members"] for name, info in manifest.items()}
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--unrar", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.unrar)
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
