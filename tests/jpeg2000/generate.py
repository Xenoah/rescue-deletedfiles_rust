"""Generate the JPEG 2000 references with OpenJPEG (Pillow) and FFmpeg's native encoder."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess

from PIL import Image

ROOT = Path(__file__).resolve().parent


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_image(name, size, mode, encoder="OpenJPEG", **settings):
    w, h = size
    channels = {"L": 1, "I;16": 1, "RGB": 3, "RGBA": 4}[mode]
    pattern = [(x * (c + 3) + y * (c * 5 + 1) + (x * y * 17) % 101)
               for y in range(h) for x in range(w) for c in range(channels)]
    deep = mode == "I;16"
    pixels = b"".join(struct.pack("<H", (v * 131) % 65536) for v in pattern) if deep else bytes(v % 256 for v in pattern)
    path = ROOT / name
    if encoder == "OpenJPEG":
        Image.frombytes(mode, size, pixels).save(path, format="JPEG2000",
            no_jp2=name.endswith(".j2k"), **settings)
    else:
        fmt = {"RGB": "rgb24", "L": "gray", "I;16": "gray16le", "RGBA": "rgba"}[mode]
        args = [v for key, val in settings.items() for v in ("-" + key, str(val))]
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pixel_format",
            fmt, "-video_size", f"{w}x{h}", "-i", "pipe:0", "-frames:v", "1", "-c:v",
            "jpeg2000", "-format", path.suffix[1:], *args, "-f", "image2", str(path)],
            check=True, input=pixels)
    with Image.open(path) as im:
        im.load()
        assert im.size == size
        decoded = im.tobytes()
        if not settings.get("irreversible") and settings.get("pred", "dwt53") == "dwt53":
            assert decoded == pixels, name
        return {"producer": encoder, "settings": settings, "mode": im.mode, "size": size,
                "bytes": path.stat().st_size, "sha256": digest(path.read_bytes()),
                "decoded_sha256": digest(decoded), "source_pixels_sha256": digest(pixels)}


if __name__ == "__main__":
    profiles = [
        ("mono.jp2", (65, 47), "L", {}, "OpenJPEG"),
        ("deepgray.jp2", (80, 59), "I;16", {"num_resolutions": 4}, "OpenJPEG"),
        ("rgba.jp2", (77, 53), "RGBA", {"num_resolutions": 4}, "OpenJPEG"),
        ("rgbtile.jp2", (103, 79), "RGB", {"tile_size": (40, 32), "num_resolutions": 4, "mct": 1}, "OpenJPEG"),
        ("lossy.jp2", (101, 82), "RGB", {"irreversible": True, "quality_layers": [9, 3],
                                        "progression": "RLCP", "mct": 1}, "OpenJPEG"),
        ("layers.j2k", (91, 65), "RGB", {"quality_layers": [12, 4, 1], "mct": 1}, "OpenJPEG"),
        ("precinct.j2k", (128, 80), "L", {"num_resolutions": 4, "progression": "RPCL",
                                         "precinct_size": (32, 32), "codeblock_size": (16, 16)}, "OpenJPEG"),
        ("deepgray.j2k", (73, 57), "I;16", {"num_resolutions": 3, "progression": "CPRL"}, "OpenJPEG"),
        ("ffrgb.j2k", (97, 71), "RGB", {"pred": "dwt53", "prog": "lrcp"}, "FFmpeg"),
        ("ffgray.j2k", (121, 73), "L", {"pred": "dwt53", "prog": "pcrl"}, "FFmpeg"),
    ]
    manifest = {name: write_image(name, size, mode, encoder, **settings)
                for name, size, mode, settings, encoder in profiles}
    (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
