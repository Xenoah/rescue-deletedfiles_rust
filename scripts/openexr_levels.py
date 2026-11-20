"""Read all levels and channels of the small native tiled reference images on Linux."""
import hashlib
import io
import shlex
from pathlib import Path
import subprocess

SOURCE = Path(__file__).resolve().parents[1] / "tests/openexr/read_levels.cpp"


def build_reader(binary):
    flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "OpenEXR"], text=True))
    subprocess.run(["c++", "-std=c++17", "-O2", str(SOURCE), *flags, "-o", str(binary)], check=True)
    return binary


def read_levels(binary, path):
    records = io.BytesIO(subprocess.check_output([binary, path]))
    result = {}
    while line := records.readline():
        x, y, w, h, kind, channel, size = line.decode("ascii").split()
        size = int(size)
        data = records.read(size)
        if len(data) != size or size != int(w) * int(h) * (2 if kind == "1" else 4):
            raise ValueError("incomplete native channel")
        level = result.setdefault(f"{x},{y}", {})
        if channel in level:
            raise ValueError("duplicate native channel")
        level[channel] = {"size": [int(w), int(h)], "type": int(kind), "sha256": hashlib.sha256(data).hexdigest()}
    if not result:
        raise ValueError("no native tile levels")
    return result
