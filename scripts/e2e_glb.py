"""Recover Blender GLB 2.0 files and verify the complete bytes, including embedded resources."""
import argparse
import json
import lzma
from pathlib import Path
import struct
import subprocess
import tempfile

from archive_trials import digest, recover_archives
from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import source_with_gaps
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4


GOLD = Path(__file__).resolve().parents[1] / "tests/glb"
MAGIC = b"glTF\x02\0\0\0"


def encode(doc, blob):
    payload = json.dumps(doc, separators=(",", ":")).encode()
    payload += b" " * (-len(payload) % 4)
    chunks = struct.pack("<I4s", len(payload), b"JSON") + payload
    if blob:
        chunks += struct.pack("<I4s", len(blob), b"BIN\0") + blob
    return MAGIC + struct.pack("<I", len(chunks) + 12) + chunks


def controls(binary, root, files):
    data = files["cube.glb"]
    split = 20 + struct.unpack_from("<I", data, 12)[0]
    original = json.loads(data[20:split])
    blob = data[split + 8:]
    cases = dict(truncated=data[:-4], reversed_chunks=data[:16] + b"BIN\0" + data[20:],
                 oversized_json=data[:12] + struct.pack("<I", 0xfffffffc) + data[16:],
                 malformed_json=data[:20] + b"!" + data[21:],
                 wrong_bin_length=data[:split] + struct.pack("<I", 4) + data[split + 4:],
                 unknown_chunk=data[:split + 4] + b"TEST" + data[split + 8:],
                 json_only_declares_buffer=encode(original, b""))
    edits = {
        "external_buffer": lambda d: d["buffers"][0].update(uri="external.bin"),
        "external_texture": lambda d: d.update(images=[dict(uri="external.png")]),
        "wrong_buffer_length": lambda d: d["buffers"][0].update(byteLength=len(blob) + 1),
        "wrong_asset_version": lambda d: d["asset"].update(version="1.0"),
        "required_extension": lambda d: d.update(extensionsRequired=["VENDOR_unknown"]),
        "range_before_eof": lambda d: d["bufferViews"][0].update(byteOffset=len(blob)),
        "wrong_accessor": lambda d: d["meshes"][0]["primitives"][0]["attributes"].update(POSITION=999),
        "out_of_view": lambda d: d["accessors"][0].update(count=2**62),
        "unknown_component": lambda d: d["accessors"][0].update(componentType=12345),
        "unaligned": lambda d: d["accessors"][0].update(byteOffset=1),
        "node_cycle": lambda d: d["nodes"][0].update(children=[0]),
        "wrong_node": lambda d: d["scenes"][0].update(nodes=[999]),
    }
    for name, mutate in edits.items():
        doc = json.loads(data[20:split])
        mutate(doc)
        cases[name] = encode(doc, blob)
    # Two-byte MAT3 columns use eight-byte steps; final-column alignment padding may be omitted.
    matrix = dict(asset={"version": "2.0"}, buffers=[{"byteLength": 22}],
                  bufferViews=[{"buffer": 0, "byteLength": 22}],
                  accessors=[{"bufferView": 0, "componentType": 5123, "count": 1, "type": "MAT3"}])
    matrix_bytes = encode(matrix, bytes(range(22)) + bytes(2))
    good = root / "matrix.glb"
    good.write_bytes(matrix_bytes)
    _, verified = candidates(binary, good, root / "matrix-scan", ["--extension", "glb"])
    check(len(verified) == 1 and verified[0]["record_sha256"] == digest(matrix_bytes), verified)
    cases["nonzero_bin_padding"] = matrix_bytes[:-1] + b"!"
    invalid_bounds = json.loads(data[20:split])
    invalid_bounds["accessors"][0]["min"] = [{"nested": "invalid"}]
    cases["malformed_accessor_bounds"] = encode(invalid_bounds, blob)
    for name, sample in cases.items():
        path = root / (name + ".raw")
        path.write_bytes(sample)
        _, rows = candidates(binary, path, root / ("scan-" + name), ["--extension", "glb"], expected=5)
        check(len(rows) == 1 and not rows[0]["supported"], (name, rows))
    # Alter one position value to a different finite coordinate, preserving container structure.
    view = original["bufferViews"][original["accessors"][original["meshes"][0]["primitives"][0]["attributes"]["POSITION"]]["bufferView"]]
    changed = bytearray(data)
    changed[split + 8 + view.get("byteOffset", 0)] ^= 1
    path = root / "edited.raw"
    path.write_bytes(changed)
    sess, rows = candidates(binary, path, root / "scan-changed", ["--extension", "glb"])
    check(len(rows) == 1 and rows[0]["record_sha256"] == digest(changed) != digest(data), rows)
    recovered = run(binary, "recover", "--session", sess, "--id", rows[0]["id"], "--destination", root/"different", "--json")
    check(Path(recovered["results"][0]["path"]).read_bytes() == changed, recovered)
    checksums = root / "original.sha256"
    checksums.write_text(digest(data) + "  cube.glb\n")
    matched = run(binary, "match-hashes", "--report", recovered["report"], "--checksums", checksums, "--json")["comparison"]
    check(matched["counts"]["not_listed"] == 1 and matched["counts"]["matched"] == 0, matched)
    first = files["uv-sphere.glb"]
    good_at = ((len(first) + 1023) // 512) * 512
    path = root / "gap.raw"
    path.write_bytes(first.ljust(good_at, b"\0") + data + bytes(4096))
    mapped = source_with_gaps(binary, path, root / "gap-map", [(4096, 512)])
    scan = run(binary, "scan", "--source", mapped, "--mode", "carve", "--extension", "glb",
               "--work-dir", root / "scan-gap", "--json", expected=5)
    rows = run(binary, "results", "--session", scan["session"], "--json")["candidates"]
    check([r["record_sha256"] for r in rows if r["supported"]] == [digest(data)], rows)
    return dict(rejected=sorted(cases), padded_matrix=digest(matrix_bytes), changed=digest(changed), original=digest(data), gap_survivor=digest(data))


def verify(binary, root, native=False, blender=None):
    manifest = json.loads((GOLD / "manifest.json").read_text(encoding="utf-8"))
    files = {n:(GOLD / "files" / n).read_bytes() for n in manifest}
    check(all(len(b) == manifest[n]["bytes"] and digest(b) == manifest[n]["sha256"] for n, b in files.items()), "GLB originals")
    result = {}
    layouts = [("fat32", make_deleted_fat), ("ext4", lambda p, f: make_deleted_ext4(p, f, block_size=4096))] if native else [("adjacent", None)]
    for name, create in layouts:
        folder = root / name
        folder.mkdir()
        if create:
            disk = create(folder, files)
        else:
            disk = folder / "adjacent.raw"
            disk.write_bytes(b"!" * ((1 << 20) - 3) + b"".join(files.values()) + bytes(72))
        restored = folder / "blender-input"
        restored.mkdir()
        def collect(path, original):
            (restored / original).write_bytes(path.read_bytes())
        result[name] = recover_archives(binary, folder, disk, files, "glb", collect)
        if blender:
            def read_meshes(source, target):
                subprocess.run([blender, "-b", "-t", "1", "--factory-startup", "--python-exit-code", "1", "--python",
                                GOLD / "blender_read.py", "--", target, source], check=True, stdout=subprocess.DEVNULL, timeout=120)
                return json.loads(target.read_text(encoding="utf-8"))
            before = read_meshes(GOLD / "files", folder / "before.json")
            after = read_meshes(restored, folder / "after.json")
            check(before == after and set(after) == set(files), name)
            result[name + "_blender"] = after
    for label, evidence in json.loads((GOLD / "disks.json").read_text(encoding="utf-8")).items():
        folder = root / ("linux-unlink-" + label)
        folder.mkdir()
        disk = folder / "unmounted.raw"
        data = lzma.decompress((GOLD / (label + ".raw.xz")).read_bytes())
        check(len(data) == evidence["bytes"] and digest(data) == evidence["sha256"], label)
        disk.write_bytes(data)
        result["linux_unlink_" + label] = recover_archives(binary, folder, disk, files, "glb")
    result["controls"] = controls(binary, root, files)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--blender", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.native, args.blender)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
