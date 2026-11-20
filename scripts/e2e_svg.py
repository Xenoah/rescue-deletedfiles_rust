"""Recover exact SVG/SVGZ originals and distinguish a delimited XML root from a complete file."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / "tests/svg"
TRAILING_NEWLINE = {"cairo.svg", "inkscape.svg"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def render(reader, path, output):
    from PIL import Image
    subprocess.run([reader, "--output", output, path], check=True)
    with Image.open(output) as img:
        rgba = img.convert("RGBA")
        check(rgba.size == (32, 24), path)
        return sha(rgba.tobytes())


def recover(binary, disk, root, files, renderer):
    expected = {n: b[:-1] if n in TRAILING_NEWLINE else b for n, b in files.items()}
    by_hash = {sha(data): name for name, data in expected.items()}
    check(len(by_hash) == len(expected), "duplicate test artwork")
    before = sha(disk.read_bytes())
    pixels = {}
    for label, extra in [("ordinary", []), ("index", ["--indexed"])]:
        session, rows = candidates(binary, disk, root / label, ["--extension", "svg", "--extension", "svgz", *extra])
        check(len(rows) == len(files) and all(r["supported"] for r in rows), rows)
        check({r["record_sha256"] for r in rows} == set(by_hash), rows)
        ids = [arg for r in rows for arg in ("--id", r["id"])]
        restored = run(binary, "recover", "--session", session, *ids, "--destination", root / (label + "-out"), "--json")["results"]
        check(len(restored) == len(files), restored)
        for row in restored:
            name = by_hash[row["sha256"]]
            path = Path(row["path"])
            data = path.read_bytes()
            check(path.suffix == Path(name).suffix and data == expected[name] and sha(data) == row["sha256"], name)
            if name in TRAILING_NEWLINE:
                check(data + b"\n" == files[name] and sha(data) != sha(files[name]), name)
            if renderer:
                actual = render(renderer, path, root / "recovered.png")
                original = render(renderer, GOLD / name, root / "original.png")
                check(actual == original, name)
                pixels[name] = actual
        print(label, len(files) - len(TRAILING_NEWLINE), "whole SVG/SVGZ originals and two XML root prefixes matched", flush=True)
    check(sha(disk.read_bytes()) == before, disk)
    result = dict(whole={n: sha(b) for n, b in files.items() if n not in TRAILING_NEWLINE},
                  root_prefix={n: sha(expected[n]) for n in sorted(TRAILING_NEWLINE)})
    if pixels:
        result["pixels_match_original"] = sorted(pixels)
    return result


def controls(binary, root, sample):
    end = b"</svg>"
    wrong_namespace = sample.replace(b"http://www.w3.org/2000/svg", b"urn:example:other")
    xml = b'<?xml version="1.0"?>'
    cases = {
        "missing_root_end": sample[:-4], "wrong_namespace": wrong_namespace,
        "unbound_prefix": sample.replace(b"<rect", b"<z:rect"),
        "duplicate_attribute": sample.replace(b'<rect x="3"', b'<rect x="9" x="3"'),
        "mismatched_element": sample.replace(end, b"</g>"),
        "unknown_entity": sample.replace(end, b"&unknown;" + end),
        "invalid_utf8": sample.replace(end, b"\xff" + end),
        "too_deep": sample.replace(end, b"<g>" * 257 + b"</g>" * 257 + end),
        "DTD": xml + b'<!DOCTYPE svg [<!ENTITY unused "value">]>' + sample,
        "unsupported_encoding": b'<?xml version="1.0" encoding="ISO-8859-1"?>' + sample,
        "not_svg": xml + b"<root/>",
    }
    rejected = []
    for name, bad in cases.items():
        for ext, data in [("svg", bad), ("svgz", gzip.compress(bad, mtime=0))]:
            disk = root / f"{name}.{ext}"
            disk.write_bytes(data)
            session = root / (name + "-" + ext)
            s = subprocess.run([binary, "scan", "--source", disk, "--mode", "carve", "--extension", ext,
                                "--work-dir", session, "--json"], check=False, capture_output=True)
            check(s.returncode in (0, 5), s.stderr)
            rows = run(binary, "results", "--session", json.loads(s.stdout)["session"], "--json")["candidates"]
            check(not any(row["record"] == 0 and row["supported"] for row in rows), (name, rows))
            rejected.append(name + "." + ext)
    gz = gzip.compress(sample, mtime=0)
    for label, value in [("trailer-crc", gz[:-8] + bytes(4) + gz[-4:]), ("trailer-length", gz[:-4] + bytes(4)),
                         ("truncated", gz[:-2]), ("trailing-non-XML", gzip.compress(sample + b"junk", mtime=0)),
                         ("two-roots", gzip.compress(sample + sample, mtime=0))]:
        disk = root / (label + ".svgz")
        disk.write_bytes(value)
        _, rows = candidates(binary, disk, root / label, ["--extension", "svgz"], expected=5)
        check(rows and all(not row["supported"] for row in rows), label)
        rejected.append(label)
    for ext, data in [("svg", sample), ("svgz", gz)]:
        disk = root / ("readmap-" + ext + ".raw")
        prefix = 4096 - len(data) // 2
        disk.write_bytes(bytes(prefix) + data + bytes(8192))
        acquired = run(binary, "image", "--source", disk, "--block-size", "4096", "--destination", root / ("acquire-" + ext), "--json")
        image = Path(acquired["image"])
        mark_gap(image, 4096)
        _, rows = candidates(binary, image, root / ("gap-" + ext), ["--extension", ext], expected=5)
        check(not any(r["supported"] for r in rows), rows)
        rejected.append("unread-payload-" + ext)
    # A structurally valid color change alters the file hash and the rendered result.
    changed = sample.replace(b"#2a56a0", b"#aa56a0")
    check(changed != sample, sample)
    disk = root / "changed.svg"
    disk.write_bytes(changed)
    session, rows = candidates(binary, disk, root / "changed-scan", ["--extension", "svg"])
    check(len(rows) == 1 and rows[0]["record_sha256"] == sha(changed), rows)
    known = root / "original.sha256"
    known.write_text(sha(sample) + "  rectangle.svg\n")
    restored = run(binary, "recover", "--session", session, "--all", "--destination", root / "changed-out", "--json")
    comparison = run(binary, "match-hashes", "--report", restored["report"], "--checksums", known, "--json")["comparison"]
    check(comparison["counts"]["not_listed"] == 1, comparison)
    return dict(refused=sorted(rejected), changed_valid_svg=dict(original=sha(sample), changed=sha(changed), match="not_listed"))


def verify(binary, root, renderer=None):
    manifest = json.loads((GOLD / "manifest.json").read_text())
    files = {n: (GOLD / n).read_bytes() for n in manifest}
    for n, b in files.items():
        check(sha(b) == manifest[n]["sha256"] and len(b) == manifest[n]["bytes"], n)
    layouts = [("fat32", make_deleted_fat), ("ext4", make_deleted_ext4)] if renderer else [("adjacent", None)]
    result = {}
    for label, creator in layouts:
        folder = root / label
        folder.mkdir()
        if creator:
            disk = creator(folder, files)
        else:
            disk = folder / "adjacent.raw"
            disk.write_bytes(b"!" * ((1 << 20) - 2) + b"".join(files.values()))
        result[label] = recover(binary, disk, folder, files, renderer)
        if label == "fat32":
            scanned = run(binary, "scan", "--source", disk, "--work-dir", folder / "fat-metadata",
                          "--estimate-fat-contiguous", "--json")
            rows = run(binary, "recover", "--session", scanned["session"], "--all",
                       "--destination", folder / "fat-recovered", "--json")["results"]
            lookup = {sha(b): n for n, b in files.items()}
            metadata = {}
            for row in rows:
                path = Path(row["path"])
                data = path.read_bytes()
                name = lookup[row["sha256"]]
                check(data == files[name] and sha(data) == row["sha256"], name)
                metadata[name] = row["extent_evidence"]
            check(set(metadata) == set(files) and metadata["inkscape.svg"] == "estimated_contiguous", metadata)
            result["fat32_metadata_evidence"] = metadata
        if label == "ext4":
            restored = folder / "metadata"
            run(binary, "filesystem", "--source", disk, "--fs-type", "ext4", "--all", "--destination", restored, "--json")
            names = {sha(b): n for n, b in files.items()}
            values = {}
            for p in restored.rglob("*"):
                if p.is_file():
                    data = p.read_bytes()
                    if name := names.get(sha(data)):
                        values[name] = data
            check(values == files, (values.keys(), files.keys()))
            result["ext4_metadata"] = {n: sha(b) for n, b in values.items()}
    result.update(controls(binary, root, files["rectangle.svg"]))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--rsvg-convert", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.rsvg_convert)
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8")
