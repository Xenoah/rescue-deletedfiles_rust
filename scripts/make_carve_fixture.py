"""Independent PNG bytes from Python struct/zlib, embedded in a regular raw file."""
import struct
import zlib
from pathlib import Path

SIGNATURE = b'\x89PNG\r\n\x1a\n'


def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def png(width=9, height=7, color=2, depth=8, interlace=False, split=False, filter_type=0):
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color]
    passes = [(0, 0, 1, 1)] if not interlace else [
        (0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4),
        (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    raw = bytearray()
    for x, y, dx, dy in passes:
        w = len(range(x, width, dx))
        h = len(range(y, height, dy))
        if not w or not h:
            continue
        row_size = (w * channels * depth + 7) // 8
        bpp = max(1, (channels * depth + 7) // 8)
        previous = bytes(row_size)
        for row in range(h):
            current = bytes((i + row) % (2**depth if color == 3 else 256) for i in range(row_size))
            filtered = bytearray()
            for i, value in enumerate(current):
                a = current[i-bpp] if i >= bpp else 0
                b = previous[i]
                c = previous[i-bpp] if i >= bpp else 0
                p = a+b-c
                pa, pb, pc = abs(p-a), abs(p-b), abs(p-c)
                predictor = [0, a, b, (a+b)//2, a if pa <= pb and pa <= pc else b if pb <= pc else c][filter_type]
                filtered.append((value-predictor) % 256)
            raw.extend(bytes([filter_type])+filtered)
            previous = current
    header = struct.pack('>IIBBBBB', width, height, depth, color, 0, 0, int(interlace))
    payload = SIGNATURE + chunk(b'IHDR', header)
    if color == 3:
        payload += chunk(b'PLTE', bytes(v for i in range(2**depth) for v in (i, i, i)))
    payload += chunk(b'tEXt', b'Generator\0Rescue independent test fixture')
    compressed = zlib.compress(raw)
    parts = [compressed] if not split else [compressed[:3], b'', compressed[3:]]
    payload += b''.join(chunk(b'IDAT', part) for part in parts)
    return payload + chunk(b'IEND', b'')


def build(path: Path):
    values = [png(), png(color=6, depth=16, interlace=True), png(color=3, depth=2, split=True)]
    offsets = [123, 1024*1024-3, 2*1024*1024+11]
    data = bytearray(3*1024*1024)
    truth = {}
    for offset, value in zip(offsets, values):
        data[offset:offset+len(value)] = value
        truth[f'png-{offset:016x}'] = value
    broken = bytearray(png()); broken[-1] ^= 1
    data[4097:4097+len(broken)] = broken
    path.write_bytes(data)
    return truth


if __name__ == '__main__':
    import argparse
    p = argparse.ArgumentParser()
    p.add_argument('image', type=Path)
    args = p.parse_args()
    print({key: len(value) for key, value in build(args.image).items()})
