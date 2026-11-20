"""Generate and independently decode every MIP/RIP level with the OpenEXR SDK."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

from generate import pixels
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from openexr_levels import build_reader, read_levels


def generate(folder):
    manifest = json.loads((folder / "manifest.json").read_text())
    with tempfile.TemporaryDirectory() as tmp:
        reader = build_reader(Path(tmp) / "read_levels")
        for stem, source, levels, rounding, sizes, compression in [
                ("miphalf", "rgbazip", "-m", "-d", (19, 17), "zip"),
                ("mipup", "rgbzips", "-m", "-u", (23, 12), "piz"),
                ("mipfloat", "ffrgb", "-m", "-u", (32, 19), "pxr24"),
                ("ripdown", "halfb44", "-r", "-d", (32, 32), "b44"),
                ("ripup", "depthpiz", "-r", "-u", (18, 16), "zip")]:
            path = folder / (stem + ".exr")
            subprocess.run(["exrmaketiled", levels, rounding, "-t", *map(str,sizes), "-z", compression,
                            str(folder / (source + ".exr")), str(path)], check=True)
            manifest[path.name] = {"producer": "OpenEXR exrmaketiled", "compression": compression,
                                   "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                   "pixels": pixels(path), "levels": read_levels(reader, path)}
    (folder / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    generate(Path(sys.argv[1]))
