"""Compare RTF recovery with the original bytes, including Pandoc documents and binary payloads."""
import argparse
import hashlib
from pathlib import Path
import tempfile
from e2e import check, run

ROOT = Path(__file__).resolve().parent.parent


def samples():
    binary = b'}{\\\0\xff}\\{'
    return {
        'corpus': (ROOT / 'tests/corpus/files/sample.rtf').read_bytes(),
        'minimal': br'{\rtf1}',
        'nested': br'{\rtf1\ansi{\fonttbl{\f0 Arial;}}{\b {\i inner \{literal\} \\}}}',
        'numeric': br"{\rtf1\ansi\uc1\u-10179?\u-8704?\par \'f1\~done}",
        'binary': br'{\rtf1{\*\objdata\bin' + str(len(binary)).encode() + b' ' + binary + b'}}',
        'long': br'{\rtf1\ansi ' + b'long text ' * 7400 + br'\par end}',
        'table': (ROOT / 'tests/rtf/table.rtf').read_bytes(),
        'unicode': (ROOT / 'tests/rtf/unicode.rtf').read_bytes(),
    }


def verify(binary, root):
    files = samples()
    disk = root / 'documents.raw'
    src = bytearray(b'\xa7' * (1024 * 1024 - 3))
    offsets = {}
    for name, data in files.items():
        offsets[len(src)] = name
        src.extend(data)
    disk.write_bytes(src)
    expected = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
    for mode, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'rtf',
                   '--work-dir', root / mode, '--json', *flags)
        candidates = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(candidates) == len(offsets) and {c['record'] for c in candidates} == offsets.keys(), candidates)
        for c in candidates:
            name = offsets[c['record']]
            check(c['supported'] and c['size'] == len(files[name]) and c['record_sha256'] == expected[name], c)
            row, = run(binary, 'recover', '--session', scan['session'], '--id', c['id'],
                       '--destination', root / (mode + '-out'), '--json')['results']
            check(Path(row['path']).read_bytes() == files[name] and row['sha256'] == expected[name], row)
        print(mode, len(files), 'RTF documents byte-exact', flush=True)
    negatives = [
        br'{\rtf11}', br'{\rtf1 unclosed', br'{\rtf1 \bin-3 abc}', br'{\rtf1 \bin payload}',
        br'{\rtf1 \bin100 short}', br"{\rtf1 \'fZ}", br"{\rtf1 \abc999999999999}",
        br'{\rtf1 ' + b'{' * 256 + b'}' * 257, b'{\\rtf1 \x00}',
    ]
    for i, data in enumerate(negatives):
        broken = root / f'broken-{i}.raw'
        broken.write_bytes(data)
        scan = run(binary, 'scan', '--mode', 'carve', '--source', broken, '--extension', 'rtf',
                   '--work-dir', root / f'bad-{i}', '--json', expected=5)
        c, = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(not c['supported'] and c['record'] == 0, c)
    check(disk.read_bytes() == src, 'source changed')
    return expected


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    with tempfile.TemporaryDirectory() as directory:
        verify(parser.parse_args().binary.resolve(), Path(directory))
