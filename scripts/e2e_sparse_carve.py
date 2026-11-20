"""Check file signatures on both sides of zero-filled scan windows and bounded ranges."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile

from e2e import check, run

TESTS = Path(__file__).resolve().parents[1] / 'tests'
MIB = 1 << 20


def verify(binary, root):
    disk = root / 'sparse.raw'
    files = TESTS / 'corpus/files'
    placements = [
        (MIB - 3, files / 'sample.mp4'),
        (3*MIB - 1, files / 'sample.ico'),
        (4*MIB + 13, files / 'sample.gif'),
        (6*MIB - 258, TESTS / 'tar/compact.tar'),
        (8*MIB - 2, files / 'sample.jpg'),
        (9*MIB + 37, files / 'sample.pdf'),
        (11*MIB - 5, files / 'sample.png'),
        (13*MIB, files / 'sample.zip'),
    ]
    with disk.open('wb') as image:
        for offset, source in placements:
            image.seek(offset)
            image.write(source.read_bytes())
        image.truncate(16*MIB)
    # Reject a PNG truncated at a caller-selected boundary, retaining its signature.
    png_at, png = placements[-2]
    methods = {}
    for indexed in (False, True):
        method = 'indexed' if indexed else 'ordinary'
        flags = ['--indexed'] if indexed else []
        def scan(label, *options, expected=0):
            return run(binary, 'scan', '--source', disk, '--mode', 'carve', '--work-dir',
                       root / (method+'-'+label), '--json', *flags, *options, expected=expected)
        session = scan('all', '--range-start', 7)
        check(session.get('index', session)['scanned_records'] == disk.stat().st_size - 7, session)
        candidates = run(binary, 'results', '--session', session['session'], '--json')['candidates']
        rows = {c['record']: c for c in candidates if c['supported']}
        ids = []
        truth = {}
        for offset, source in placements:
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            check(offset in rows and rows[offset]['record_sha256'] == digest, (method, source, rows.get(offset)))
            ids.extend(['--id', rows[offset]['id']])
            truth[digest] = source
        restored = run(binary, 'recover', '--session', session['session'], *ids, '--destination',
                       root / (method+'-out'), '--json')['results']
        check(len(restored) == len(placements), restored)
        for row in restored:
            actual = Path(row['path']).read_bytes()
            check(hashlib.sha256(actual).hexdigest() == row['sha256'] and actual == truth[row['sha256']].read_bytes(), row)
        short = scan('short', '--extension', 'png', '--range-start', png_at, '--range-end',
                     png_at + png.stat().st_size - 1, expected=5)
        c, = run(binary, 'results', '--session', short['session'], '--json')['candidates']
        check(not c['supported'] and c['record'] == png_at, c)
        zero = scan('zero-rule', '--signature-rule', 'zeros:00000000:size=4',
                    '--range-start', 222, '--range-end', 226)
        c, = run(binary, 'results', '--session', zero['session'], '--json')['candidates']
        r, = run(binary, 'recover', '--session', zero['session'], '--id', c['id'], '--destination',
                 root / (method+'-zeros'), '--json')['results']
        check(c['record'] == 222 and Path(r['path']).read_bytes() == bytes(4)
              and r['sha256'] == hashlib.sha256(bytes(4)).hexdigest(), r)
        methods[method] = {p.name: h for h,p in sorted(truth.items())}
    return methods


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', type=Path, required=True)
    ap.add_argument('--output', type=Path)
    args = ap.parse_args()
    with tempfile.TemporaryDirectory() as folder:
        result = verify(args.binary.resolve(), Path(folder))
    if args.output:
        args.output.write_text(json.dumps(result, sort_keys=True, indent=2)+'\n', encoding='utf-8')
    print('8 file formats recovered on both sides of sparse windows in both scans, truncated PNG rejected and all-zero custom rule preserved')
