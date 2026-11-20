"""Generate native OpenEXR and FFmpeg originals and record hashes of individual decoded channels."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

import numpy as np
import OpenEXR as exr


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pixels(path):
    with exr.File(str(path), separate_channels=True) as image:
        result = []
        for part in image.parts:
            channels = {}
            for name, channel in part.channels.items():
                arr = channel.pixels
                canonical = np.ascontiguousarray(arr, dtype=arr.dtype.newbyteorder("<"))
                channels[name] = {"shape": list(arr.shape), "dtype": canonical.dtype.str,
                                  "sha256": sha(canonical.tobytes())}
            result.append(channels)
        return result


def make(folder):
    folder.mkdir(parents=True, exist_ok=True)
    originals = {}
    yy, xx = np.indices((67, 91))
    def values(name, index):
        if name == "objectID":
            return np.ascontiguousarray((xx * 127 + yy + 65536 * index).astype(np.uint32))
        v = np.sin((xx + index) / 17) * np.cos((yy - index) / 11) * 4 + (xx % 13 == 0) * 3
        return v.astype(np.float32 if name in ("Z", "floatY") else np.float16)

    combinations = [
        ("luma", "NO", ["Y"], None),
        ("rlegrey", "RLE", ["Y"], None),
        ("rgbzips", "ZIPS", ["R", "G", "B"], None),
        ("rgbazip", "ZIP", ["R", "G", "B", "A"], None),
        ("depthpiz", "PIZ", ["Z", "objectID"], None),
        ("floatpxr", "PXR24", ["Z", "objectID", "R"], None),
        ("halfb44", "B44", ["R", "G", "B"], None),
        ("alphab44", "B44A", ["Y", "A"], None),
        ("tileszip", "ZIP", ["Y", "objectID"], (23, 19)),
        ("tilespiz", "PIZ", ["R", "G", "B", "Z"], (32, 16)),
        ("tilerle", "RLE", ["Y"], (12, 17)),
        ("negwin", "ZIP", ["R", "objectID", "Z"], None),
        ("descend", "ZIPS", ["left.R", "left.G", "left.B", "right.R"], None),
        ("longname", "ZIP", ["custom.layer.with.a.channel.name.over.31.characters", "Z"], None),
    ]
    for index, (stem, compression, names, tile) in enumerate(combinations):
        path = folder / (stem + ".exr")
        header = {"compression": getattr(exr, compression + "_COMPRESSION"),
                  "type": exr.tiledimage if tile else exr.scanlineimage,
                  "comments": "Rescue OpenEXR reference " + stem}
        if tile:
            desc = exr.TileDescription()
            desc.xSize, desc.ySize = tile
            header["tiles"] = desc
        if stem == "negwin":
            low = np.array([-18, 31], dtype=np.int32)
            header["dataWindow"] = (low, low + np.array([90, 66], dtype=np.int32))
            header["displayWindow"] = (np.array([-40, 10], dtype=np.int32), np.array([100, 120], dtype=np.int32))
        if stem == "descend":
            header["lineOrder"] = exr.DECREASING_Y
        with exr.File(header, {name: values(name, index + j) for j, name in enumerate(names)}) as image:
            image.write(str(path))
        originals[path.name] = {"producer": "OpenEXR " + exr.__version__, "compression": compression,
                                "sha256": sha(path.read_bytes()), "pixels": pixels(path)}

    for index, (stem, fmt, compression, channels) in enumerate([
            ("ffgray", "grayf32le", "zip16", ["floatY"]),
            ("ffrgb", "gbrpf32le", "rle", ["G", "B", "R"]),
            ("ffrgba", "gbrapf32le", "zip1", ["G", "B", "R", "A"])]):
        data = b"".join(values(name, index + j + 2).astype("<f4").tobytes() for j,name in enumerate(channels))
        path = folder / (stem + ".exr")
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", fmt, "-s", "91x67",
                        "-i", "pipe:0", "-frames:v", "1", "-c:v", "exr", "-compression", compression,
                        "-threads", "1", str(path)], input=data, check=True)
        ffmpeg = subprocess.check_output(["ffmpeg", "-version"], text=True).splitlines()[0]
        originals[path.name] = {"producer": ffmpeg, "compression": compression, "sha256": sha(path.read_bytes()), "pixels": pixels(path)}
    (folder / "manifest.json").write_text(json.dumps(originals, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    make(parser.parse_args().directory)
