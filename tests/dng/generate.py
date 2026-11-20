"""Write Bayer DNG originals using tifffile, numpy and two lossless-JPEG encoders."""

import hashlib
import json
from pathlib import Path

import imagecodecs
import numpy as np
import tifffile

ROOT = Path(__file__).resolve().parent
CASES = [
    (8, "<", False, "raw"),
    (16, "<", False, "raw"),
    (16, ">", False, "raw"),
    (8, ">", True, "raw"),
    (16, "<", True, "raw"),
    (16, ">", True, "raw"),
    (10, ">", False, "packed"),
    (12, "<", False, "packed"),
    (14, ">", False, "packed"),
    (12, "<", False, "lj92"),
    (14, ">", True, "lj92"),
    (16, "<", True, "lj92"),
    (8, ">", False, "turbo"),
    (12, "<", True, "turbo"),
    (16, ">", False, "turbo"),
]


def chunks(pixels, tiled, rows):
    height, width = pixels.shape
    if not tiled:
        for y in range(0, height, rows):
            yield pixels[y : y + rows]
        return
    for y in range(0, height, 16):
        for x in range(0, width, 16):
            tile = np.zeros((16, 16), dtype=pixels.dtype)
            area = pixels[y : y + 16, x : x + 16]
            tile[: area.shape[0], : area.shape[1]] = area
            yield tile


def make(i, bits, order, tiled, encoder):
    h, w = [(32, 64), (53, 80), (52, 77)][i % 3]
    pattern = ["RGGB", "BGGR", "GRBG", "GBRG"][i % 4]
    peak = (1 << bits) - 1
    dtype = np.uint8 if bits == 8 else np.uint16
    y, x = np.indices((h, w), dtype=np.uint32)
    pixels = ((x * 173 + y * 317 + (x * y % 251) * 137 + i * 79) % peak).astype(dtype)
    identity = (1, 1, 0, 1, 0, 1, 0, 1, 1, 1, 0, 1, 0, 1, 0, 1, 1, 1)
    tags = [
        (50706, "B", 4, (1, 4, 0, 0), False),
        (50707, "B", 4, (1, 1, 0, 0), False),
        (50708, "s", 0, "Rescue test sensor " + str(i), False),
        (33421, "H", 2, (2, 2), False),
        (33422, "B", 4, tuple("RGB".index(c) for c in pattern), False),
        (50710, "B", 3, (0, 1, 2), False),
        (50711, "H", 1, 1, False),
        (50721, "2i", 9, identity, False),
        (50778, "H", 1, 21, False),
        (50714, "H", 1, 0, False),
        (50717, "I", 1, peak, False),
        (50728, "2I", 3, (1, 1, 1, 1, 1, 1), False),
    ]
    name = f"{encoder}-{bits}-{i}.dng"
    path = ROOT / name
    options = dict(
        photometric=32803, metadata=None, extratags=tags, byteorder=order, software="Rescue DNG fixtures"
    )
    rows = h if encoder in ("lj92", "turbo") else 11
    geometry = {"tile": (16, 16)} if tiled else {"rowsperstrip": rows}
    if encoder == "raw":
        tifffile.imwrite(path, pixels, **geometry, **options)
    else:
        encoded = []
        for block in chunks(pixels, tiled, rows):
            if encoder == "packed":
                planes = (block[:, :, None] >> np.arange(bits - 1, -1, -1)) & 1
                chunk = np.packbits(planes.reshape(block.shape[0], -1), axis=1, bitorder="big").tobytes()
            elif encoder == "lj92":
                chunk = imagecodecs.ljpeg_encode(block.astype(np.uint16), bitspersample=bits)
            else:
                chunk = imagecodecs.jpeg_encode(
                    block, lossless=True, predictor=3, bitspersample=bits, colorspace="GRAY"
                )
            encoded.append(chunk)
        tifffile.imwrite(
            path,
            iter(encoded),
            shape=pixels.shape,
            dtype=dtype,
            bitspersample=bits,
            compression=1 if encoder == "packed" else 7,
            **geometry,
            **options,
        )
    return name, {
        "bytes": path.stat().st_size,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "width": w,
        "height": h,
        "bits": bits,
        "pattern": pattern,
        "endian": "little" if order == "<" else "big",
        "tiled": tiled,
        "encoding": encoder,
        "samples_u16le_sha256": hashlib.sha256(pixels.astype("<u2").tobytes()).hexdigest(),
    }


if __name__ == "__main__":
    manifest = dict(make(i, *args) for i, args in enumerate(CASES))
    (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
