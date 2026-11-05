"""Generate only synthetic JPEGs with the independent Pillow/libjpeg encoder.

The integration tests use committed bytes and need no Pillow dependency.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import random
import struct
from PIL import Image, __version__ as pillow_version


def segment(marker, data):
    return bytes([255, marker]) + struct.pack('>H', len(data)+2) + data


def generate(root):
    root.mkdir(parents=True, exist_ok=True)
    entries = []
    rng = random.Random(20261105)

    def add(name, data, **options):
        with Image.open(io.BytesIO(data)) as decoded:
            decoded.load()
            width, height = decoded.size
            mode = decoded.mode
        path = root/(name+'.jpg')
        path.write_bytes(data)
        entries.append({'file':path.name, 'bytes':len(data), 'sha256':hashlib.sha256(data).hexdigest(),
                        'width':width, 'height':height, 'mode':mode, **options})

    def encode(mode, size, **options):
        channels = len(mode) if mode != 'L' else 1
        image = Image.frombytes(mode, size, rng.randbytes(size[0]*size[1]*channels))
        out = io.BytesIO()
        image.save(out, format='JPEG', quality=88, **options)
        return out.getvalue()

    for mode in ['L', 'RGB', 'CMYK']:
        for progressive in [False, True]:
            for sampling in ([0, 1, 2] if mode == 'RGB' else [0]):
                options = {'progressive':progressive, 'subsampling':sampling, 'optimize':True}
                add(f'{mode.lower()}-{int(progressive)}-{sampling}', encode(mode, (37, 29), **options), **options)
    for size in [(1,1), (7,9), (16,16), (127,65)]:
        for progressive in [False, True]:
            options = {'progressive':progressive, 'subsampling':2}
            add(f'edge-{size[0]}x{size[1]}-{int(progressive)}', encode('RGB', size, **options), **options)
    for interval in [1,3,7]:
        for progressive in [False,True]:
            options = {'progressive':progressive, 'restart_marker_blocks':interval}
            data = encode('RGB', (81,65), **options)
            assert b'\xff\xdd' in data and b'\xff\xd0' in data
            add(f'restart-{interval}-{int(progressive)}', data, **options)
    sample = (root/'rgb-0-0.jpg').read_bytes()
    add('sof1', sample.replace(b'\xff\xc0', b'\xff\xc1', 1), extended_sequential=True)
    # Explicit SOI/EOI in metadata must not truncate the outer JPEG. Its valid
    # thumbnail remains independently discoverable by an all-byte scan.
    thumbnail = (root/'edge-1x1-0.jpg').read_bytes()
    app = b'Exif\x00\x00synthetic\xff\xd9\x00' + thumbnail + b'\x00tail'
    add('embedded-thumbnail', sample[:2]+segment(0xe1,app)+sample[2:], embedded_thumbnail='edge-1x1-0.jpg')
    add('comment-fill', sample[:2]+b'\xff'+segment(0xfe,b'comment \xff\xd9 and \xff\x00')+sample[2:], marker_fill=True)
    # 16-bit DQT is valid for extended sequential DCT; use small values so the
    # independent decoder can confirm the same pixels.
    at=sample.index(b'\xff\xdb');length=struct.unpack_from('>H',sample,at+2)[0]
    body=sample[at+4:at+2+length]
    wide=bytes([0x10|body[0]])+b''.join(bytes([0,n]) for n in body[1:])
    modified=sample[:at]+segment(0xdb,wide)+sample[at+2+length:]
    add('quant16',modified.replace(b'\xff\xc0',b'\xff\xc1',1), quantization_bits=16)
    for progressive in [False,True]:
        add(f'true-rgb-{int(progressive)}',encode('RGB',(33,19),keep_rgb=True,progressive=progressive), true_rgb=True, progressive=progressive)
    # Independent minimal encoder: flat blocks, explicit components in separate
    # sequential scans. Only DC=0 and AC=EOB are needed (one bit each).
    dqt=segment(0xdb,b'\x00'+bytes([1])*64)
    dht=segment(0xc4,b'\x00\x01'+bytes(15)+b'\x00'+b'\x10\x01'+bytes(15)+b'\x00')
    for count in [1,3]:
        sof=segment(0xc0,bytes([8])+struct.pack('>HHB',8,16,count)+b''.join(bytes([i+1,0x11,0]) for i in range(count)))
        scans=b''.join(segment(0xda,bytes([1,i+1,0,0,63,0]))+b'\x0f' for i in range(count))
        add(f'separate-{count}',b'\xff\xd8'+dqt+sof+dht+scans+b'\xff\xd9',separate_components=count,encoder='Minimal flat-block encoder; independently Pillow decoded')
    manifest={'schema_version':1,'provenance':'Synthetic deterministic random pixels only; no third-party images or user data.',
              'generator':f'Pillow {pillow_version} / libjpeg {Image.core.jpeglib_version}',
              'validation':'Every image independently decoded by Pillow with truncated-image loading disabled.',
              'files':entries}
    (root/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n',encoding='utf-8')
    print(f'Generated and independently decoded {len(entries)} JPEG fixtures')


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,default=Path('tests/jpeg'));a=p.parse_args();generate(a.output)
