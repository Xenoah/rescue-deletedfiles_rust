"""Recover finite Windows Media containers and compare complete files and decoded streams."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / "tests/asf"
FILTER = ["--extension", "wma", "--extension", "wmv", "--extension", "asf"]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def decode(path, streams):
    info = subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "stream=codec_name,codec_type",
                                    "-of", "json", path], timeout=30)
    check(json.loads(info)["streams"] == streams, path)
    outputs = []
    for index, stream in enumerate(streams):
        options = (["-f", "s16le", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1"]
                   if stream["codec_type"] == "audio" else ["-f", "rawvideo", "-pix_fmt", "rgb24", "-threads", "1"])
        process = subprocess.run(["ffmpeg", "-v", "error", "-xerror", "-i", path, "-map", f"0:{index}", *options, "-"],
                                 capture_output=True, check=True, timeout=30)
        check(process.stdout and not process.stderr.strip(), (path, index, process.stderr))
        outputs.append(process.stdout)
    return outputs


def recover(binary, disk, root, files, manifest, decoded, sparse_tails):
    originals = {sha(b): n for n, b in files.items()}
    expected = {h for h, n in originals.items() if n not in sparse_tails}
    for label, flags in [("ordinary", []), ("indexed", ["--indexed"])]:
        session, rows = candidates(binary, disk, root / label, FILTER + flags, expected=5 if sparse_tails else 0)
        check(len(rows) == len(files), rows)
        rejected = [r for r in rows if not r["supported"]]
        check(len(rejected) == len(sparse_tails), rejected)
        rows = [r for r in rows if r["supported"]]
        check({r["record_sha256"] for r in rows} == expected, rows)
        check(all(r["supported"] and r["extent_evidence"] == "file_structure" and
                  Path(r["name"]).suffix == Path(originals[r["record_sha256"]]).suffix for r in rows), rows)
        ids = [arg for row in rows for arg in ("--id", row["id"])]
        results = run(binary, "recover", "--session", session, *ids, "--destination", root / (label + "-out"), "--json")["results"]
        check(len(results) == len(expected), results)
        for row in results:
            name = originals[row["sha256"]]
            path = Path(row["path"])
            data = path.read_bytes()
            check(row["status"] == "completed" and data == files[name] and sha(data) == manifest[name]["sha256"], name)
            if decoded:
                check(decode(path, manifest[name]["streams"]) == decoded[name], name)
    return {n: sha(b) for n, b in files.items() if n not in sparse_tails}


def extract_metadata(binary, disk, root, layout, files, manifest, decoded):
    if layout == "fat32":
        scan = run(binary, "scan", "--source", disk, "--work-dir", root / "fat-metadata", "--json")
        rows = run(binary, "results", "--session", scan["session"], "--json")["candidates"]
        check(len(rows) == len(files) and not any(r["supported"] for r in rows), rows)
        scan = run(binary, "scan", "--source", disk, "--work-dir", root / "fat-estimate",
                   "--estimate-fat-contiguous", "--json")
        result = run(binary, "recover", "--session", scan["session"], "--all",
                     "--destination", root / "metadata-out", "--json")["results"]
        check(len(result) == len(files) and all(r["extent_evidence"] == "estimated_contiguous" for r in result), result)
        paths = [Path(r["path"]) for r in result]
    else:
        output = root / "metadata-out"
        run(binary, "filesystem", "--source", disk, "--fs-type", "ext4", "--all", "--destination", output, "--json")
        paths = [p for p in output.rglob("*") if p.is_file()]
    recovered = {sha(p.read_bytes()): p for p in paths}
    for name, data in files.items():
        path = recovered.get(sha(data))
        check(path and path.read_bytes() == data and decode(path, manifest[name]["streams"]) == decoded[name], (layout, name))
    return {name: sha(data) for name, data in files.items()}


def controls(binary, root, files):
    # All offsets below address fields from the independent writers' captured originals.
    data = files["silent.wmv"]
    header_end = int.from_bytes(data[16:24], "little")
    file_obj = 30
    stream = data.index(bytes.fromhex("9107dcb7b7a9cf118ee600c00c205365"))
    index = header_end + int.from_bytes(data[header_end + 16:header_end + 24], "little")
    packet = header_end + 50
    codecs = data.index(bytes.fromhex("4052d1861d31d011a3a400a0c90348f6"))
    text = data.index(bytes.fromhex("40a4d0d207e3d21197f000a0c95ea850"))
    def change(pos, replacement):
        return data[:pos] + replacement + data[pos + len(replacement):]
    cases = {
        "real-streamable-asfmux": (GOLD / "unsupported/broadcast.wma").read_bytes(),
        "truncated-header": data[:100], "truncated-last-index": data[:-1], "truncated-packet": data[:packet + 700],
        "header-object-count": change(24, struct.pack("<I", 2)), "header-reserved": change(29, b"\0"),
        "short-object": change(file_obj + 16, struct.pack("<Q", 19)),
        "file-length-too-small": change(file_obj + 40, struct.pack("<Q", 48)),
        "broadcast-flag": change(file_obj + 88, b"\x03"), "unequal-packet-limits": change(file_obj + 96, struct.pack("<I", 511)),
        "packet-count": change(file_obj + 56, struct.pack("<Q", 4)),
        "encrypted-stream": change(stream + 72, b"\x01\x80"), "reserved-stream-number": change(stream + 72, bytes(2)),
        "unknown-stream-type": change(stream + 24, bytes(16)), "codec-extra-length": change(stream + 64, bytes(4)),
        "bitmap-width-mismatch": change(stream + 93, bytes(4)),
        "data-file-id": change(header_end + 24, b"\xff"), "data-packet-count": change(header_end + 40, bytes(8)),
        "data-object-size": change(header_end + 16, struct.pack("<Q", 50)), "data-reserved": change(header_end + 49, b"\0"),
        "unlisted-payload-stream": change(packet + 11, b"\x87"), "unbounded-replication": change(packet + 17, b"\xff"),
        "media-object-size": change(packet + 18, bytes(4)), "unknown-error-correction": change(packet, b"\x83"),
        "index-file-id": change(index + 24, b"\xff"), "index-interval": change(index + 40, bytes(8)),
        "index-entry-count": change(index + 52, bytes(4)), "index-packet-outside": change(index + 56, bytes([255]) * 4),
        "index-oversize-span": change(index + 60, b"\xff\xff"), "late-empty-index-span": change(len(data)-2, bytes(2)),
        "unknown-trailer": change(index, bytes(16)), "codec-list-reserved": change(codecs + 24, bytes(16)),
        "codec-list-count": change(codecs + 40, struct.pack("<I", 128)), "description-name-length": change(text + 26, b"\xff\xff"),
        "unsupported-header-object": change(codecs, bytes(16)),
    }
    for label, contents in cases.items():
        path = root / (label + ".raw")
        path.write_bytes(contents)
        _, rows = candidates(binary, path, root / (label + "-scan"), FILTER, expected=5)
        check(rows and not any(r["supported"] for r in rows), (label, rows))
    padded = (len(data) + 4095) // 4096 * 4096
    for label, offset in [("unread-inside", 4096), ("unread-after", padded)]:
        disk = root / (label + ".raw")
        disk.write_bytes(bytes(padded - len(data)) + data + bytes(8192))
        acquired = run(binary, "image", "--source", disk, "--block-size", "4096", "--destination", root / (label + "-acquired"), "--json")
        image = Path(acquired["image"])
        mark_gap(image, offset)
        session, rows = candidates(binary, image, root / (label + "-scan"), FILTER, expected=5)
        if label == "unread-inside":
            check(not any(r["supported"] for r in rows), rows)
        else:
            check(len(rows) == 1 and rows[0]["supported"] and rows[0]["record_sha256"] == sha(data), rows)
            row, = run(binary, "recover", "--session", session, "--all", "--destination", root / "after-out", "--json")["results"]
            check(Path(row["path"]).read_bytes() == data and row["sha256"] == sha(data), row)
    # A valid metadata change is structurally indistinguishable from the source absent a known hash.
    changed = data.replace("復元試験".encode("utf-16-le"), "復元確認".encode("utf-16-le"))
    check(data != changed, "fixture metadata")
    disk = root / "modified.wmv"
    disk.write_bytes(changed)
    session, rows = candidates(binary, disk, root / "changed", FILTER)
    check(len(rows) == 1 and rows[0]["record_sha256"] == sha(changed), rows)
    report = run(binary, "recover", "--session", session, "--all", "--destination", root / "changed-out", "--json")
    known = root / "known.sha256"
    known.write_text(sha(data) + "  silent.wmv\n")
    match = run(binary, "match-hashes", "--report", report["report"], "--checksums", known, "--json")["comparison"]
    check(match["counts"]["not_listed"] == 1, match)
    return dict(refused=sorted([*cases, "unread-inside"]), unread_after=sha(data), changed_title=dict(original=sha(data), changed=sha(changed), match="not_listed"))


def verify(binary, root, native=False):
    manifest = json.loads((GOLD / "manifest.json").read_text(encoding="utf-8"))
    files = {name: (GOLD / name).read_bytes() for name in manifest}
    for name, data in files.items():
        check(len(data) == manifest[name]["bytes"] and sha(data) == manifest[name]["sha256"], name)
    decoded = {n: decode(GOLD / n, m["streams"]) for n, m in manifest.items()} if native else {}
    result = {}
    layouts = ("fat32", "ext4") if native else ("adjacent",)
    for layout in layouts:
        folder = root / layout
        folder.mkdir()
        if layout == "fat32":
            disk = make_deleted_fat(folder, files)
        elif layout == "ext4":
            disk = make_deleted_ext4(folder, files, block_size=4096)
        else:
            disk = folder / "adjacent.raw"
            disk.write_bytes(b"!" * ((1 << 20) - 7) + b"".join(files.values()))
        before = sha(disk.read_bytes())
        sparse_tails = {"lowrate.wma", "mp3.asf"} if layout == "ext4" else set()
        result[layout] = recover(binary, disk, folder, files, manifest, decoded, sparse_tails)
        if sparse_tails:
            result["sparse_tails_refused_by_carving"] = sorted(sparse_tails)
        if native:
            result[layout + "_metadata"] = extract_metadata(binary, disk, folder, layout, files, manifest, decoded)
        else:
            for ext in ("asf", "wmv", "wma"):
                session, rows = candidates(binary, disk, folder / ("filter-" + ext), ["--extension", ext])
                expected = {sha(b) for n, b in files.items() if ext == "asf" or n.endswith("." + ext)}
                check(len(rows) == len(expected) and {r["record_sha256"] for r in rows} == expected, (ext, rows))
                check(all(r["name"].endswith("." + ext) for r in rows), ext)
                restored = run(binary, "recover", "--session", session, "--all", "--destination", folder / ("out-" + ext), "--json")["results"]
                check({sha(Path(row["path"]).read_bytes()) for row in restored} == expected, ext)
        check(sha(disk.read_bytes()) == before, disk)
        print(layout, len(result[layout]), "ASF originals carved exactly,", len(files) if native else 0, "metadata recoveries", flush=True)
    if native:
        result["decoded_streams_match"] = {n: [s["codec_name"] for s in m["streams"]] for n, m in manifest.items()}
    result.update(controls(binary, root, files))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.native)
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8")
