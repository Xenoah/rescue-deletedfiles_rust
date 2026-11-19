"""Carve nine ImageMagick Netpbm images and check source bytes, hashes and native decoded pixels."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile

from e2e import check, run
from e2e_extended_filesystems import command
from e2e_imaging import mark_gap

GOLD = Path(__file__).resolve().parents[1] / 'tests/netpbm'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def pixels(path):
    return command('ffmpeg', '-v', 'error', '-i', path, '-frames:v', '1',
                   '-f', 'rawvideo', '-pix_fmt', 'rgba64be', '-').stdout


def make_deleted_ext4(root, files):
    disk = root / 'ext4.raw'
    with disk.open('wb') as out:
        out.truncate(16 << 20)
    command('mke2fs', '-q', '-t', 'ext4', '-F', disk)
    for name, data in files.items():
        (root / name).write_bytes(data)
        command('debugfs', '-w', '-R', f'write {name} /{name}', disk, cwd=root)
        command('debugfs', '-R', f'dump /{name} before-{name}', disk, cwd=root)
        check((root / ('before-' + name)).read_bytes() == data, name)
    for name in files:
        command('debugfs', '-w', '-R', f'rm /{name}', disk)
    removed = command('fls', '-dFrp', '-f', 'ext4', disk).stdout.decode()
    check(all('\t' + name + '\n' in removed for name in files), removed)
    return disk


def verify(binary, root, native=False):
    manifest = json.loads((GOLD / 'manifest.json').read_text())
    files = {name: (GOLD / name).read_bytes() for name in manifest}
    lookup = {digest(data): name for name, data in files.items()}
    check(all(meta['sha256'] in lookup and lookup[meta['sha256']] == n for n, meta in manifest.items()), manifest)
    references = {}
    if native:
        disk = make_deleted_ext4(root, files)
        for name, meta in manifest.items():
            references[name] = pixels(root / name)
            check(len(references[name]) == meta['width'] * meta['height'] * 8, name)
    else:
        disk = root / 'images.raw'
        disk.write_bytes(b'!' * ((1 << 20) - 1) + b''.join(files.values()))
    before = digest(disk.read_bytes())
    for mode, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'pnm',
                   '--work-dir', root / mode, '--json', *flags, timeout=180)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(rows) == len(files) and {r['record_sha256'] for r in rows} == set(lookup), rows)
        ids = [v for r in rows for v in ('--id', r['id'])]
        results = run(binary, 'recover', '--session', scan['session'], *ids,
                      '--destination', root / (mode + '-out'), '--json')['results']
        check(len(results) == len(files), results)
        for row in results:
            path = Path(row['path'])
            data = path.read_bytes()
            name = lookup[row['sha256']]
            check(data == files[name] and digest(data) == row['sha256'], name)
            if native:
                check(pixels(path) == references[name], name)
        print(mode, len(rows), 'actual ext4 Netpbm deletions' if native else 'Netpbm images',
              'match bytes and SHA-256', flush=True)
    check(digest(disk.read_bytes()) == before, 'source changed')
    if not native:
        cases = [
            b'P1\n5 1\n01210\n', b'P2\n2 1\n21\n21 22\n', b'P3\n2 3\n255\n1 2 3',
            b'P4\n9 2\n\xfe\x0f\x20', b'P4\n0 2\n\0\0', b'P5\n2 1\n1023\n\0\4\4\0',
            b'P5\n2 1\n100\n\x65\0', b'P6\n9999999999 9999999999\n65535\n',
            b'P5\n1 1\n255\r\n\x42', b'P6\n1 1\n65536\n\0\0\0\0\0\0',
            files['alpha.pam'].replace(b'DEPTH 4', b'DEPTH 3'),
            files['alpha.pam'].replace(b'HEIGHT 17', b'WIDTH 17'),
            files['alpha.pam'].replace(b'ENDHDR', b'ENDING'),
            files['alpha16.pam'][:-1],
        ]
        for i, data in enumerate(cases):
            bad = root / f'bad-{i}.raw'
            bad.write_bytes(data)
            scan = run(binary, 'scan', '--source', bad, '--mode', 'carve', '--extension', 'pnm',
                       '--work-dir', root / f'bad-{i}', '--json', expected=5)
            rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
            check(rows and all(not r['supported'] for r in rows), (i, rows))

        # Whitespace, '#' and NUL can be the first raw sample. Never parse them as header text.
        edge = [b'P5\n#dimensions\n4 1\n255\n\x0a# \0',
                b'P2\n3 1\n19\n1 #inside raster\n5 19\r\n', b'P1\n5 1\n01010\n',
                b'P7\r\n#reordered\nDEPTH 1\nHEIGHT 1\nWIDTH 3\nMAXVAL 1\nTUPLTYPE BLACKANDWHITE\nENDHDR\r\n\0\1\0']
        edge_disk = root / 'edges.raw'
        edge_disk.write_bytes(b''.join(edge))
        scan = run(binary, 'scan', '--source', edge_disk, '--mode', 'carve', '--extension', 'pnm',
                   '--work-dir', root / 'edges', '--json')
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check({r['record_sha256'] for r in rows} == {digest(d) for d in edge}, rows)
        clean = files['rgb8.ppm']
        cross = files['alpha16.pam']
        source = root / 'coverage.raw'
        source.write_bytes(b'!' * (4096 - len(clean)) + clean + bytes(4096) + cross + bytes(4096))
        acquired = run(binary, 'image', '--source', source, '--destination', root / 'image',
                       '--block-size', '4096', '--json')
        image = Path(acquired['image'])
        mark_gap(image, 4096)
        mark_gap(image, 12288)
        scan = run(binary, 'scan', '--source', image, '--mode', 'carve', '--extension', 'pnm',
                   '--work-dir', root / 'coverage', '--json', expected=5)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check({r['record_sha256'] for r in rows if r['supported']} == {digest(clean)}, rows)
        check(any(r['record'] == 8192 and not r['supported'] for r in rows), rows)
        print(len(cases), 'invalid images refused; four raster boundaries checked; failed sector refused', flush=True)
    return {n: m['sha256'] for n, m in manifest.items()}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.native)
