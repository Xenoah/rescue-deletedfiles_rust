"""Recover independently encoded Zstandard/LZ4 frames, including checksums and adjacent members."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run

FIXTURES = Path(__file__).resolve().parents[1] / 'tests' / 'frame-archives.json'


def raw_cases():
    mixed = bytes(range(251)) * 1040
    noise = b''.join(hashlib.sha256(bytes([n])).digest() for n in range(35))
    return {
        'empty.zst': (b'', ['--check']),
        'declared.zst': (mixed[:260], ['--check']),
        'multiblock.zst': (mixed, ['--check']),
        'no-check.zst': (noise, ['--no-check']),
        'stream.zst': (mixed[:4611], ['--check']),
        'empty.lz4': (b'', ['-B4']),
        'linked.lz4': (mixed, ['-B4', '-BD', '-BX', '--content-size']),
        'independent.lz4': (mixed + mixed[:5], ['-B4', '-BI', '-BX']),
        'no-check.lz4': (noise, ['-B4', '--no-frame-crc']),
        'both-checks.lz4': (mixed[:284], ['-B4', '-BX', '--content-size']),
    }


def reference(root):
    frames = {}
    for name, (data, flags) in raw_cases().items():
        program = 'zstd' if name.endswith('.zst') else 'lz4'
        source = root / (name + '.input')
        source.write_bytes(data)
        stream = name == 'stream.zst'
        cmd = [program, '-q', '-c', *flags, '-' if stream else str(source)]
        encoded = subprocess.check_output(cmd, input=data if stream else None)
        decoded = subprocess.check_output([program, '-q', '-d', '-c', '-'], input=encoded)
        check(decoded == data, name)
        frames[name] = encoded
    return frames


def golden_frames():
    return {k: base64.b64decode(v, validate=True) for k, v in json.loads(FIXTURES.read_text()).items()}


def verify(binary, root, frames):
    disk = root / 'frames.raw'
    image = bytearray(b'\xa9' * (1024 * 1024 - 2))
    offsets = {}
    for name, frame in frames.items():
        offsets[len(image)] = name
        image.extend(frame)
    disk.write_bytes(image)
    for label, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'zst',
                   '--extension', 'lz4', '--work-dir', root / label, '--json', *flags)
        found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(found) == len(offsets) and {c['record'] for c in found} == offsets.keys(), found)
        for candidate in found:
            name = offsets[candidate['record']]
            data = frames[name]
            expected = hashlib.sha256(data).hexdigest()
            check(candidate['supported'] and candidate['size'] == len(data)
                  and candidate['record_sha256'] == expected, (name, candidate))
            row, = run(binary, 'recover', '--session', scan['session'], '--id', candidate['id'],
                       '--destination', root / (label + '-out'), '--json')['results']
            check(row['sha256'] == expected and Path(row['path']).read_bytes() == data, row)
        print(label, len(found), 'single frames byte-exact', flush=True)
    def flip(data, pos):
        changed = bytearray(data)
        changed[pos] ^= 1
        return changed
    def size_field(zstd):
        descriptor = zstd[4]
        check(descriptor & 32 and descriptor >> 6 == 1, 'expected 2-byte Zstandard content size')
        return 5
    zstd = frames['declared.zst']
    lz4 = frames['both-checks.lz4']
    negatives = [('checksum.zst', flip(zstd, -1)), ('truncated.zst', frames['multiblock.zst'][:-6]),
                 ('content-size.zst', flip(zstd, size_field(zstd))),
                 ('invalid-block.zst', b'\x28\xb5\x2f\xfd\x20\x00\x07\x00\x00'),
                 ('checksum.lz4', flip(lz4, -1)), ('header-check.lz4', flip(lz4, 14)),
                 ('block-check.lz4', flip(lz4, -9)), ('truncated.lz4', lz4[:-7]),
                 ('end.lz4', frames['no-check.lz4'][:-4])]
    for i, (name, data) in enumerate(negatives):
        path = root / name
        path.write_bytes(data)
        scan = run(binary, 'scan', '--source', path, '--mode', 'carve', '--extension', path.suffix[1:],
                   '--work-dir', root / str(i), '--json', expected=5)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(rows and all(not c['supported'] for c in rows), (name, rows))
    check(disk.read_bytes() == image, 'source changed')
    return {name: hashlib.sha256(data).hexdigest() for name, data in frames.items()}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path)
    p.add_argument('--independent', action='store_true')
    p.add_argument('--write-fixtures', action='store_true')
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        if args.write_fixtures:
            fixture = {k: base64.b64encode(v).decode('ascii') for k, v in reference(root).items()}
            FIXTURES.write_text(json.dumps(fixture, indent=2) + '\n', encoding='utf-8')
        else:
            if args.independent:
                frames = reference(root)
            else:
                frames = golden_frames()
            check(frames.keys() == raw_cases().keys(), 'expected ten reference frames')
            verify(args.binary.resolve(), root, frames)
