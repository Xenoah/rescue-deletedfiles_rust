"""Recover independent JPEG 2000 files and compare their complete bytes, digests and pixels."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import tempfile

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_netpbm import make_deleted_ext4
from e2e_imaging import mark_gap

GOLD = Path(__file__).resolve().parents[1] / 'tests/jpeg2000'
FILTER = ['--extension', 'jp2', '--extension', 'j2k']


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pixels(path):
    from PIL import Image
    with Image.open(path) as image:
        image.load()
        return list(image.size), image.mode, sha(image.tobytes())


def candidates(binary, image, work, args, expected=0):
    scan = run(binary, 'scan', '--source', image, '--mode', 'carve', '--work-dir', work,
               *args, '--json', expected=expected, timeout=180)
    rows = run(binary, 'results', '--session', scan['session'], '--limit', 100, '--json')['candidates']
    return scan['session'], rows


def recover(binary, image, folder, files, manifest, native):
    lookup = {sha(data): name for name, data in files.items()}
    before = sha(image.read_bytes())
    for label, flags in [('ordinary', []), ('indexed', ['--indexed'])]:
        session, rows = candidates(binary, image, folder / label, FILTER + flags)
        check(len(rows) == len(files) and {r['record_sha256'] for r in rows} == lookup.keys(), rows)
        check(all(r['supported'] and r['extent_evidence'] == 'file_structure'
                  and Path(r['name']).suffix == Path(lookup[r['record_sha256']]).suffix for r in rows), rows)
        out = run(binary, 'recover', '--session', session, '--all', '--destination', folder / (label + '-out'),
                  '--json', timeout=180)['results']
        check(len(out) == len(files), out)
        for row in out:
            name = lookup[row['sha256']]
            path = Path(row['path'])
            data = path.read_bytes()
            check(row['status'] == 'completed' and data == files[name] and sha(data) == manifest[name]['sha256'], row)
            if native:
                verify_pixels(path, manifest[name])
    check(sha(image.read_bytes()) == before, 'source changed')
    print(image.name, len(files), 'JPEG 2000 originals matched in ordinary and indexed recovery', flush=True)


def verify_pixels(path, item):
    check(pixels(path) == (item['size'], item['mode'], item['decoded_sha256']), path)


def damaged(binary, folder, files):
    boxed = files['mono.jp2']
    data = files['deepgray.j2k']
    sot = data.index(b'\xff\x90')
    cod = data.index(b'\xff\x52')
    qcd = data.index(b'\xff\x5c')
    def edit(src, at, value):
        return src[:at] + value + src[at+len(value):]
    cases = {
        'soc-without-image': data[:8],
        'profile': edit(data, 6, b'\x40\x00'),
        'dimensions': edit(data, 8, bytes(4)),
        'grid-over-limit': edit(data, 8, struct.pack('>II', 8192, 8192)),
        'tile-count': edit(data, 24, struct.pack('>II', 1, 1)),
        'precision': edit(data, 42, b'\x20'),
        'sampling': edit(data, 43, b'\x00'),
        'progression': edit(data, cod + 5, b'\x05'),
        'layers': edit(data, cod + 6, b'\xff\xff'),
        'levels': edit(data, cod + 9, b'\x20'),
        'codeblock': edit(data, cod + 10, b'\x0d'),
        'quantization': edit(data, qcd + 4, b'\xff'),
        'quantization-length': edit(data, qcd + 2, b'\x00\x04'),
        'tile-index': edit(data, sot + 4, b'\x00\x01'),
        'tile-length': edit(data, sot + 6, b'\xff' * 4),
        'tile-part': edit(data, sot + 10, b'\x01'),
        'missing-sod': edit(data, sot + 12, b'\xff\x64'),
        'premature-eoc': edit(data, sot + 19, b'\xff\xd9'),
        'broken-entropy': edit(data, sot + 14, b'\xff\x7f' * 16),
        'missing-eoc': data[:-2],
        'brand': edit(boxed, 20, b'jpx '),
        'unknown-color': edit(boxed, boxed.index(b'colr') + 10, b'\x22'),
        'header-width': edit(boxed, boxed.index(b'ihdr') + 8, bytes(4)),
        'header-precision': edit(boxed, boxed.index(b'ihdr') + 14, b'\x08'),
        'box-length': edit(boxed, boxed.index(b'jp2c') - 4, struct.pack('>I', len(boxed))),
        'truncated-box': boxed[:-1],
        'metadata-truncated': boxed + struct.pack('>I4s', 32, b'uuid') + b'xx',
        'nested-stream': boxed + boxed[boxed.index(b'jp2c')-4:],
    }
    tile = files['rgbtile.jp2']
    first = tile.index(b'\xff\x90')
    second = first + int.from_bytes(tile[first+6:first+10], 'big')
    cases['duplicate-tile'] = edit(tile, second + 4, b'\x00\x00')
    extended = files['longbox.jp2']
    start = extended.index(b'jp2c') - 4
    cases['extended-length-overflow'] = edit(extended, start + 8, bytes([255]) * 8)
    cases['extended-length-too-small'] = edit(extended, start + 8, struct.pack('>Q', 15))
    cases['open-ended-truncation'] = files['lastbox.jp2'][:-2]
    cases['metadata-count'] = boxed + struct.pack('>I4s', 8, b'free') * 257
    cases['short-uuid'] = boxed + struct.pack('>I4s', 9, b'uuid') + b'z'
    for label, value in cases.items():
        p = folder / (label + '.raw')
        p.write_bytes(value)
        ext = 'jp2' if value[:4] == boxed[:4] else 'j2k'
        _, rows = candidates(binary, p, folder / label, ['--extension', ext], expected=5)
        check(rows and all(not r['supported'] for r in rows), (label, rows))
    # Strict acquisition boundaries: one file ends exactly at a bad sector; another crosses it.
    disk = folder / 'gaps.raw'
    prefix = bytes(4096 - len(boxed))
    disk.write_bytes(prefix + boxed + bytes(8192 - 100) + data + bytes(8192))
    copied = run(binary, 'image', '--source', disk, '--destination', folder / 'capture',
                 '--block-size', 4096, '--json')
    mark_gap(copied['image'], 4096)
    mark_gap(copied['image'], 12288)
    for label, flags in [('ordinary', []), ('indexed', ['--indexed'])]:
        _, rows = candidates(binary, copied['image'], folder / ('gap-' + label), FILTER + flags, expected=5)
        check(len(rows) == 2 and any(r['supported'] and r['record_sha256'] == sha(boxed) for r in rows)
              and any(not r['supported'] and r['record'] == 12288 - 100 for r in rows), rows)
    return sorted(cases)


def changed_comment(binary, root, files):
    # A real codestream comment can change without altering any pixel. Compare the output
    # against the original whole-file digest to expose what structural checks cannot detect.
    source = files['mono.jp2']
    pos = source.index(b'Created by OpenJPEG')
    altered = source[:pos] + b'c' + source[pos + 1:]
    disk = root / 'altered.jp2'
    disk.write_bytes(altered)
    session, rows = candidates(binary, disk, root / 'altered-scan', ['--extension', 'jp2'])
    check(len(rows) == 1 and rows[0]['supported'] and rows[0]['record_sha256'] == sha(altered), rows)
    saved = run(binary, 'recover', '--session', session, '--all', '--destination', root / 'altered-output', '--json')
    path = Path(saved['results'][0]['path'])
    check(path.read_bytes() == altered and sha(altered) != sha(source), path)
    sums = root / 'original.sha256'
    sums.write_text(sha(source) + '  mono.jp2\n')
    result = run(binary, 'match-hashes', '--report', saved['report'], '--checksums', sums, '--json')['comparison']
    check(result['counts'] == {'matched': 0, 'not_listed': 1, 'not_comparable': 0}, result)
    return {'original': sha(source), 'altered': sha(altered), 'comparison': 'not_listed'}


def verify(binary, root, native=False):
    manifest = json.loads((GOLD / 'manifest.json').read_text())
    files = {name: (GOLD / name).read_bytes() for name in manifest}
    hashes = {n: sha(b) for n, b in files.items()}
    check(all(hashes[n] == m['sha256'] and len(files[n]) == m['bytes'] for n, m in manifest.items()), manifest)
    if native:
        for name in files:
            verify_pixels(GOLD / name, manifest[name])
        for label, create in [('fat32', make_deleted_fat), ('ext4', make_deleted_ext4)]:
            folder = root / label
            folder.mkdir()
            recover(binary, create(folder, files), folder, files, manifest, True)
        return {'originals': hashes, 'filesystems': ['fat32', 'ext4'],
                'decoded': {name: m['decoded_sha256'] for name, m in manifest.items()}}
    disk = root / 'adjacent.raw'
    disk.write_bytes(b'!' * ((1 << 20) - 2) + b''.join(files.values()))
    recover(binary, disk, root, files, manifest, False)
    for ext in ('jp2', 'jpc', 'j2c'):
        _, rows = candidates(binary, disk, root / ext, ['--extension', ext])
        if ext == 'jp2':
            want = {h for n, h in hashes.items() if n.endswith('.jp2')}
        else:
            # A raw-only search can independently salvage embedded JP2 codestreams.
            want = {sha(b[b.index(b'\xff\x4f\xff\x51'):b.index(b'\xff\xd9', b.index(b'\xff\x4f\xff\x51'))+2])
                    for b in files.values()}
        check({r['record_sha256'] for r in rows} == want, (ext, rows))
    return {'originals': hashes, 'refused': damaged(binary, root, files),
            'changed_comment': changed_comment(binary, root, files)}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--native', action='store_true')
    p.add_argument('--output', type=Path)
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        result = verify(a.binary.resolve(), Path(scratch), a.native)
    if a.output:
        a.output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n', encoding='utf-8')
