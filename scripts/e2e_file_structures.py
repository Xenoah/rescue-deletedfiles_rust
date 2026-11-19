"""Exercise five file structures through both scan engines and the public restore path."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile

from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus'
EXTENSIONS = {'bmp', 'gif', 'sqlite', 'wav', 'webp'}


def verify(binary):
    rows = json.loads((CORPUS / 'manifest.json').read_text(encoding='utf-8'))['files']
    files = {}
    for row in rows:
        if row['extension'] in EXTENSIONS:
            data = (CORPUS / row['file']).read_bytes()
            check(hashlib.sha256(data).hexdigest() == row['sha256'], row['file'])
            files[row['extension']] = data
    check(files.keys() == EXTENSIONS, 'incomplete corpus')
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        image = root / 'no-filesystem.raw'
        contents = bytearray(b'\xa5' * (1024 * 1024 - 3))
        offsets = {}
        for ext, data in files.items():
            offsets[ext] = len(contents)
            contents.extend(data + b'\xa5' * 31)
        image.write_bytes(contents)
        for mode, flags in [('session', []), ('index', ['--indexed'])]:
            scan = run(binary, 'scan', '--source', image, '--mode', 'carve', '--carve-scope', 'all',
                       '--work-dir', root / (mode + '-work'), '--json', *flags)
            found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
            for ext, data in files.items():
                candidate, = [c for c in found if c['record'] == offsets[ext]]
                check(candidate['supported'] and candidate['size'] == len(data)
                      and candidate['extent_evidence'] == 'file_structure', candidate)
                result, = run(binary, 'recover', '--session', scan['session'], '--id', candidate['id'],
                              '--destination', root / (mode + '-out'), '--json')['results']
                check(Path(result['path']).read_bytes() == data
                      and result['sha256'] == hashlib.sha256(data).hexdigest(), result)
                print(mode, ext, len(data), result['sha256'])
        for ext, data in files.items():
            cut = root / (ext + '-cut.raw')
            cut.write_bytes(data[:-1])
            scan = run(binary, 'scan', '--source', cut, '--mode', 'carve', '--extension', ext,
                       '--work-dir', root / (ext + '-cut'), '--json', expected=5)
            found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
            check(any(c['record'] == 0 for c in found) and not any(c['supported'] for c in found), ext)
        check(image.read_bytes() == contents, 'source changed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    verify(parser.parse_args().binary.resolve())
