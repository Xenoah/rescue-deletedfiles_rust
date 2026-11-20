"""Capture SVG originals, gzip envelopes, whole-file hashes and a rendered pixel reference."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from PIL import Image

SVG_NAMESPACE = "http://www.w3.org/2000/svg"
BODIES = {
 'rectangle':'<rect x="3" y="4" width="12" height="8" fill="#2a56a0"/>',
 'gradient':'<defs><linearGradient id="g"><stop stop-color="#ef4822"/><stop offset="1" stop-color="#1144bb"/></linearGradient></defs><rect width="30" height="20" fill="url(#g)"/>',
 'clipped':'<defs><clipPath id="c"><circle cx="14" cy="10" r="7"/></clipPath></defs><rect width="28" height="18" fill="#4066e0" clip-path="url(#c)"/>',
 'reused':'<defs><path id="p" d="M1 1L13 2L9 8Z"/></defs><use xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="#p" fill="brown"/><use xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="#p" transform="translate(10 10)" fill="green"/>',
 'stroke':'<g transform="translate(1,2)"><path d="M0 6 Q9 -2 18 8 T28 10" fill="none" stroke="#228844" stroke-width="2"/></g>',
 'unicode':'<title>復元の図形</title><desc>グループ &amp; パス</desc><ellipse cx="17" cy="12" rx="9" ry="5" fill="#a35a80"/>',
}


def write_utf8_artwork(out):
    for name, body in BODIES.items():
        text = f'<svg xmlns="{SVG_NAMESPACE}" width="32" height="24" viewBox="0 0 32 24">{body}</svg>'
        if name == "gradient":
            text = '<?xml version="1.0" encoding="UTF-8"?>\n' + text
        elif name == "clipped":
            text = "\ufeff" + text
        elif name == "unicode":
            text = text.replace("<svg ", f'<v:svg xmlns:v="{SVG_NAMESPACE}" ').replace("</svg>", "</v:svg>")
            text = '<?xml version="1.0"?>\n' + text
        (out / f"{name}.svg").write_bytes(text.encode("utf-8"))


def export_artwork(out, inkscape, cairo_python):
    subprocess.run(
        [inkscape, out / "gradient.svg", "--export-plain-svg", "--export-type=svg",
         f"--export-filename={out / 'inkscape.svg'}"],
        check=True, capture_output=True,
    )
    script = """
import cairo, sys
surface = cairo.SVGSurface(sys.argv[1], 32, 24)
context = cairo.Context(surface)
context.set_source_rgb(.2, .5, .7)
context.arc(13, 13, 8, 0, 6.28)
context.fill()
surface.finish()
"""
    subprocess.run([cairo_python, "-c", script, out / "cairo.svg"], check=True)


def write_utf16_artwork(out):
    for tag, bom in [("le", b"\xff\xfe"), ("be", b"\xfe\xff")]:
        for name in ("rectangle", "reused", "unicode", "boundary"):
            source = "rectangle" if name == "boundary" else name
            text = (out / f"{source}.svg").read_text(encoding="utf-8-sig")
            if text.startswith("<?xml"):
                text = text.split("?>", 1)[1].lstrip()
            if name in ("rectangle", "unicode"):
                declared = "UTF-16" if name == "rectangle" else "UTF-16" + tag.upper()
                text = f'<?xml version="1.0" encoding="{declared}"?>\n' + text
            if name == "unicode":
                text = text.replace("復元", "復元🌊")
            elif name == "boundary":
                prefix, rect = text.split("<rect", 1)
                prefix += "<desc>"
                # The high and low surrogates straddle byte 4096, including the BOM.
                text = prefix + "x" * (2046 - len(prefix)) + "🌊</desc><rect" + rect
            path = out / f"{tag}-{name}.svg"
            path.write_bytes(bom + text.encode("utf-16-" + tag))
            path.with_suffix(".svgz").write_bytes(gzip.compress(path.read_bytes(), mtime=0))


def capture_manifest(out, renderer):
    manifest = {}
    with tempfile.TemporaryDirectory() as temp:
        png = Path(temp) / "vector.png"
        for path in sorted(out.glob("*.svg*")):
            data = path.read_bytes()
            subprocess.run([renderer, "--output", png, path], check=True)
            with Image.open(png) as image:
                rgba = image.convert("RGBA")
                assert rgba.size == (32, 24)
                pixels = rgba.tobytes()
            assert len(pixels) == 3072 and any(pixels)
            manifest[path.name] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                                       rgba_sha256=hashlib.sha256(pixels).hexdigest())
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cairo-python", default="/usr/bin/python3")
    parser.add_argument("--inkscape", default="inkscape")
    parser.add_argument("--rsvg-convert", default="rsvg-convert")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(exist_ok=False)
    write_utf8_artwork(out)
    export_artwork(out, args.inkscape, args.cairo_python)
    for path in sorted(out.glob("*.svg")):
        path.with_suffix(".svgz").write_bytes(gzip.compress(path.read_bytes(), compresslevel=6, mtime=0))
    write_utf16_artwork(out)
    print(f"{len(capture_manifest(out, args.rsvg_convert))} SVG/SVGZ originals captured")
