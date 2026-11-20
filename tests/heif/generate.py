"""Generate the original still photographs for the carver trials using independent encoders."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

GOLD = Path(__file__).resolve().parent


def png(path, width, height, seed, channels=3, depth=8):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    pixels = bytearray()
    for y in range(height):
        pixels.append(0)
        for x in range(width):
            for c in range(channels):
                v = (x * (13 + c) + y * (3 + seed) + (x ^ y) * seed + c * 73) % 256
                pixels.extend(bytes([v]) if depth == 8 else struct.pack('>H', v * 256 + (x * y) % 256))
    head = struct.pack('>2IBBBBB', width, height, depth, {1: 0, 3: 2, 4: 6}[channels], 0, 0, 0)
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', head) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))


def generate(root):
    originals = [
        ('odd', 79, 61, 3, 8), ('alpha', 64, 48, 4, 8), ('small', 61, 63, 3, 8),
        ('large', 144, 112, 3, 8), ('gray', 66, 56, 1, 8), ('deep', 80, 64, 3, 16),
        ('tile-01-01', 64, 64, 3, 8), ('tile-01-02', 64, 64, 3, 8),
        ('tile-02-01', 64, 64, 3, 8), ('tile-02-02', 64, 64, 3, 8),
    ]
    for seed, (name, w, h, ch, bits) in enumerate(originals, 1):
        png(root / (name+'.png'), w, h, seed, ch, bits)
    cases = {
        'odd.heic': ([], ['odd']), 'alpha.heic': ([], ['alpha']),
        'ten-bit.heic': (['-b', '10'], ['deep']), 'collection.heic': ([], ['odd', 'small']),
        'tiled.heic': (['-T'], ['tile-01-01']), 'thumb.heic': (['-t', '64'], ['large']),
        'color.avif': ([], ['odd']), 'alpha.avif': ([], ['alpha']),
        'ten-bit.avif': (['-b', '10'], ['deep']), 'gray.avif': ([], ['gray']),
        'ffmpeg.avif': ([], ['small']),
    }
    manifest = {}
    for name, (options, inputs) in cases.items():
        output = GOLD/name
        if name == 'ffmpeg.avif':
            args = ['ffmpeg', '-nostdin', '-loglevel', 'error', '-y', '-i', 'small.png',
                    '-frames:v', '1', '-pix_fmt', 'yuv420p', '-c:v', 'libaom-av1', '-cpu-used',
                    '8', '-threads', '1', '-still-picture', '1', output]
        else:
            codec = ['-A'] if name.endswith('.avif') else ['-p', 'x265:pools=none', '-p', 'x265:frame-threads=1']
            args = ['heif-enc', *codec, '-q', '65', *options, *[n+'.png' for n in inputs], '-o', output]
        subprocess.run(args, cwd=root, check=True)
        data = output.read_bytes()
        manifest[name] = {'bytes':len(data), 'sha256':hashlib.sha256(data).hexdigest(),
                          'encoder':'FFmpeg/libaom' if name=='ffmpeg.avif' else
                                    ('libheif/libaom' if name.endswith('.avif') else 'libheif/x265')}
    (GOLD/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n', encoding='utf-8')


if __name__ == '__main__':
    with tempfile.TemporaryDirectory() as tmp:
        generate(Path(tmp))
