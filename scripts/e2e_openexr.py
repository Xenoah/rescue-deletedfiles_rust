"""Recover native HDR images from adjacent bytes or actual FAT32/ext4 deletions."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import tempfile

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / "tests/openexr"
FILTER = ["--extension", "exr"]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify_pixels(path, expected):
    import OpenEXR
    import numpy as np
    with OpenEXR.File(str(path), separate_channels=True) as image:
        check(len(image.parts) == len(expected), path)
        for part, channels in zip(image.parts, expected, strict=True):
            check(part.channels.keys() == channels.keys(), path)
            for name, truth in channels.items():
                values = part.channels[name].pixels
                values = np.ascontiguousarray(values, dtype=values.dtype.newbyteorder("<"))
                check(truth == {"shape": list(values.shape), "dtype": values.dtype.str,
                                "sha256": sha(values.tobytes())}, (path, name))


def recover(binary, image, work, files, manifest, native):
    lookup = {sha(v): k for k, v in files.items()}
    before = sha(image.read_bytes())
    for mode, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
        session, rows = candidates(binary, image, work / mode, FILTER + flags)
        check(len(rows) == len(files) and {r["record_sha256"] for r in rows} == lookup.keys(), rows)
        check(all(r["supported"] and r["extent_evidence"] == "file_structure" and r["name"].endswith(".exr")
                  for r in rows), rows)
        restored = run(binary, "recover", "--session", session, "--all", "--destination", work / (mode + "-files"),
                       "--json", timeout=180)["results"]
        check(len(restored) == len(files), restored)
        for row in restored:
            name = lookup[row["sha256"]]
            path = Path(row["path"])
            data = path.read_bytes()
            check(row["status"] == "completed" and data == files[name] and sha(data) == manifest[name]["sha256"], row)
            if native:
                verify_pixels(path, manifest[name]["pixels"])
    check(sha(image.read_bytes()) == before, image)
    print(image.name, len(files), "OpenEXR originals matched in both scan modes", flush=True)


def layout(data, pos=8):
    fields = {}
    while data[pos]:
        name, pos = data[pos:data.index(0, pos)], data.index(0, pos) + 1
        end = data.index(0, pos)
        length = struct.unpack_from("<I", data, end + 1)[0]
        pos = end + 5
        fields[name.decode()] = (pos, length)
        pos += length
    return fields, pos + 1


def edit(data, pos, replacement):
    return data[:pos] + replacement + data[pos + len(replacement):]


def controls(binary, folder, files):
    data = files["rgbazip.exr"]
    fields, table = layout(data)
    chunk = struct.unpack_from("<Q", data, table)[0]
    size = struct.unpack_from("<I", data, chunk + 4)[0]
    payload = chunk + 8
    channels = fields["channels"][0]
    record = data.index(0, channels) + 1
    tile = files["tileszip.exr"]
    tf, tt = layout(tile)
    tile_header = struct.unpack_from("<Q", tile, tt)[0]
    le = lambda n: struct.pack("<I", n)
    big = lambda n: struct.pack("<Q", n)
    cases = {
        "version": edit(data, 4, le(3)),
        "deep": edit(data, 4, le(0x802)),
        "multipart_flag_without_parts": edit(data, 4, le(0x1002)),
        "reserved_flags": edit(data, 4, le(0x40002)),
        "missing_data_window": data.replace(b"dataWindow\0", b"datoWindow\0", 1),
        "attribute_size": edit(data, fields["channels"][0] - 4, le(0x70000000)),
        "duplicate_name": data[:table-1] + data[8:fields["channels"][0]+fields["channels"][1]] + data[table-1:],
        "excessive_window": edit(data, fields["dataWindow"][0]+8, le(1<<30)),
        "inverted_display_window": edit(data, fields["displayWindow"][0]+8, le(0xffffffff)),
        "channel_sample_type": edit(data, record, le(4)),
        "channel_subsampling": edit(data, record + 8, le(3)),
        "channel_reserved": edit(data, record+5, b"\x01"),
        "channel_linearity": edit(data, record+4, b"\x04"),
        "channel_order": edit(data, channels, b"z"),
        "unsupported_compression": edit(data, fields["compression"][0], b"\x0b"),
        "unsupported_line_order": edit(data, fields["lineOrder"][0], b"\x07"),
        "offset_inside_header": edit(data, table, big(12)),
        "offset_past_source": edit(data, table, big(len(data)+1024)),
        "offset_overlap": edit(data, table+8, data[table:table+8]),
        "offset_hole": edit(data, table, big(chunk+1)),
        "offset_swapped_identities": edit(data, table, data[table+8:table+16] + data[table:table+8]),
        "wrong_row": edit(data, chunk, le(1)),
        "zero_chunk_size": edit(data, chunk+4, le(0)),
        "excessive_chunk_size": edit(data, chunk+4, le(0x7fffffff)),
        "incomplete_final_block": data[:-1],
        "invalid_zlib_header": edit(data, payload, b"\0\0"),
        "adler32_mismatch": edit(data, payload+size-1, bytes([data[payload+size-1] ^ 1])),
        "zero_tile_size": edit(tile, tf["tiles"][0], le(0)),
        "unsupported_tile_level": edit(tile, tf["tiles"][0]+8, b"\x01"),
        "tile_y": edit(tile, tile_header+4, le(9)),
        "tile_level_id": edit(tile, tile_header+8, le(1)),
        "incomplete_tile": tile[:-9],
    }
    multi = files["stereo.exr"]
    first, second = layout(multi)
    other, stop = layout(multi, second)
    table_start = stop + 1
    num_left = struct.unpack_from("<I", multi, first["chunkCount"][0])[0]
    left = struct.unpack_from("<Q", multi, table_start)[0]
    right = struct.unpack_from("<Q", multi, table_start + 8*num_left)[0]
    cases.update({
        "multipart_unknown_part": edit(multi, left, le(17)),
        "multipart_wrong_valid_part": edit(multi, right, le(0)),
        "multipart_aliased_chunk": edit(multi, table_start + 8*num_left, big(left)),
        "multipart_wrong_chunk_count": edit(multi, first["chunkCount"][0], le(num_left+1)),
        "multipart_missing_count": multi.replace(b"chunkCount\0", b"otherCount\0", 1),
        "multipart_name_missing": multi.replace(b"name\0string", b"part\0string", 1),
        "multipart_invalid_name": edit(multi, first["name"][0], b"\0"),
        "multipart_type_missing": multi.replace(b"type\0string", b"none\0string", 1),
        "multipart_deep_part": edit(multi, first["type"][0], b"deepscanline "),
        "multipart_tiled_file_flag": edit(multi, 4, le(0x1202)),
        "multipart_unterminated_headers": edit(multi, stop, b"x"),
        "multipart_outside_final_block": multi[:-13],
    })
    for label, broken in cases.items():
        p = folder / (label + ".raw")
        p.write_bytes(broken)
        _, rows = candidates(binary, p, folder / label, FILTER, expected=5)
        check(rows and all(not r["supported"] for r in rows), (label, rows))
    # Adjacent good file ends at the unreadable sector. A second copy crosses a later gap.
    boundary = (len(data) + 4095) // 4096 * 4096
    second = boundary + 8192 - 100
    raw = folder / "gaps.raw"
    raw.write_bytes(bytes(boundary - len(data)) + data + bytes(second - boundary) + data + bytes(8192))
    capture = run(binary, "image", "--source", raw, "--destination", folder / "acquired",
                  "--block-size", 4096, "--json")
    mark_gap(capture["image"], boundary)
    mark_gap(capture["image"], boundary + 8192)
    for mode, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
        _, rows = candidates(binary, capture["image"], folder / ("gap-" + mode), FILTER + flags, expected=5)
        check(len(rows) == 2 and any(r["supported"] and r["record_sha256"] == sha(data) for r in rows)
              and any(not r["supported"] and r["record"] == second for r in rows), rows)
    return sorted(cases)


def sparse_ext4(binary, root, files, manifest):
    # debugfs omits all-zero 1 KiB blocks while writing these two native files.
    # The surviving extents are authoritative about both the location and the length of each hole.
    selected = {n: files[n] for n in ("stereo.exr", "mixedrow.exr")}
    disk = make_deleted_ext4(root, selected, block_size=1024)
    before = sha(disk.read_bytes())
    holes = {n: [pos for pos in range(0, len(b)-1023, 1024) if b[pos:pos+1024] == bytes(1024)]
             for n,b in selected.items()}
    check(all(holes.values()) and all(b not in disk.read_bytes() for b in selected.values()), holes)
    for mode, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
        _, rows = candidates(binary, disk, root / mode, FILTER + flags, expected=5)
        check(len(rows) == len(selected) and all(not r["supported"] for r in rows), rows)
    saved = run(binary, "filesystem", "--source", disk, "--fs-type", "ext4", "--all",
                "--destination", root / "metadata-files", "--json")["results"]
    check({r["original_path"] for r in saved} == selected.keys(), saved)
    for row in saved:
        n = row["original_path"]
        path = Path(row["path"])
        check(path.read_bytes() == selected[n] and row["sha256"] == sha(selected[n]), row)
        verify_pixels(path, manifest[n]["pixels"])
    check(sha(disk.read_bytes()) == before, disk)
    return {"zero_block_offsets": holes, "metadata_restored": {n: sha(b) for n,b in selected.items()}}


def changed_metadata(binary, root, source):
    pos = source.index(b"Rescue OpenEXR reference")
    altered = edit(source, pos, b"r")
    path = root / "changed.exr"
    path.write_bytes(altered)
    session, rows = candidates(binary, path, root / "changed-scan", FILTER)
    check(len(rows) == 1 and rows[0]["supported"], rows)
    saved = run(binary, "recover", "--session", session, "--all", "--destination", root / "changed-out", "--json")
    check(Path(saved["results"][0]["path"]).read_bytes() == altered, saved)
    sums = root / "original.sha256"
    sums.write_text(sha(source) + "  rgbazip.exr\n")
    match = run(binary, "match-hashes", "--report", saved["report"], "--checksums", sums, "--json")["comparison"]
    check(match["counts"] == {"matched": 0, "not_listed": 1, "not_comparable": 0}, match)
    return {"original": sha(source), "changed": sha(altered), "comparison": "not_listed"}


def verify(binary, root, native=False):
    manifest = json.loads((GOLD / "manifest.json").read_text())
    files = {n: (GOLD / n).read_bytes() for n in manifest}
    hashes = {n: sha(v) for n, v in files.items()}
    check(hashes == {n: m["sha256"] for n, m in manifest.items()}, "fixture drift")
    if native:
        for n in manifest:
            verify_pixels(GOLD / n, manifest[n]["pixels"])
        for label in ["fat32", "ext4"]:
            folder = root / label
            folder.mkdir()
            image = make_deleted_fat(folder, files) if label == "fat32" else make_deleted_ext4(folder, files, block_size=4096)
            recover(binary, image, folder, files, manifest, True)
        sparse = root / "sparse"
        sparse.mkdir()
        return {"originals": hashes, "filesystems": ["fat32", "ext4-4096"], "sparse_ext4": sparse_ext4(binary, sparse, files, manifest),
                "pixels": {n: m["pixels"] for n,m in manifest.items()}}
    image = root / "adjacent.raw"
    image.write_bytes(b"!" * ((1 << 20) - 2) + b"".join(files.values()))
    recover(binary, image, root, files, manifest, False)
    return {"originals": hashes, "refused": controls(binary, root, files),
            "changed_metadata": changed_metadata(binary, root, files["rgbazip.exr"])}


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--native", action="store_true")
    p.add_argument("--output", type=Path)
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(a.binary.resolve(), Path(tmp), a.native)
    if a.output:
        a.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8")
