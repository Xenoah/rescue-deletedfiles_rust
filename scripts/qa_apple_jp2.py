"""Create Apple ImageIO JP2 originals and decode each through ImageIO and independent OpenJPEG."""
from pathlib import Path
import hashlib
import json
import subprocess
from PIL import Image, features

folder = Path("apple-reference")
folder.mkdir()
manifest = {}
for i, (mode, quality) in enumerate([("L", "best"), ("RGB", "best"), ("RGB", "high"),
                                      ("L", "normal"), ("RGB", "low"), ("RGBA", "best")]):
    w, h = 97 + i * 7, 73 + i * 5
    pix = bytes((x * (c + 13) + y * (x + c * 11) + i) % 256 for y in range(h)
                for x in range(w) for c in range(Image.getmodebands(mode)))
    image = Image.frombytes(mode, (w, h), pix)
    src = folder / f"source{i}.png"
    out = folder / f"apple{i}.jp2"
    decoded = folder / f"readback{i}.png"
    image.save(src)
    subprocess.run(["sips", "-s", "format", "jp2", "-s", "formatOptions", quality, str(src), "--out", str(out)], check=True)
    subprocess.run(["sips", "-s", "format", "png", str(out), "--out", str(decoded)], check=True)
    with Image.open(decoded) as apple, Image.open(out) as opj:
        apple.load()
        opj.load()
        assert apple.size == opj.size == (w,h)
        sha = lambda data: hashlib.sha256(data).hexdigest()
        manifest[out.name] = dict(sha256=sha(out.read_bytes()), bytes=out.stat().st_size,
            size=[w,h], input_mode=mode, mode=opj.mode, source_pixels_sha256=sha(pix), quality=quality,
            apple_png_mode=apple.mode, apple_png_decoded_sha256=sha(apple.tobytes()), decoded_sha256=sha(opj.tobytes()),
            apple_source_sha256=sha(src.read_bytes()), apple_png_sha256=sha(decoded.read_bytes()),
            producer="Apple ImageIO (sips)", openjpeg=features.version("jpg_2000"))
(folder / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
print(json.dumps(manifest, indent=2))
