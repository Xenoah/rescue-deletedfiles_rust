"""Recover declared HTML5 documents and compare their source bytes and SHA-256."""
import argparse
import hashlib
from html.parser import HTMLParser
from pathlib import Path
import tempfile

from e2e import check, run
from e2e_extended_filesystems import CORPUS, command
from e2e_imaging import mark_gap


def digest(data):
    return hashlib.sha256(data).hexdigest()


def samples():
    def document(body):
        return ('<!DOCTYPE html><html lang="ja"><head><meta charset="UTF-8">'
                '<title>復元</title></head><body>' + body + '</body></html>').encode()

    return {
        'bom.html': b'\xef\xbb\xbf' + document('<p>庭の写真 &amp; 記録</p>'),
        'corpus.htm': (CORPUS / 'sample.html').read_bytes(),
        'script.html': document('<script>const sample = "</html>";</script><p>script text</p>'),
        'notes.html': document('<!-- literal </html> -->\r\n<p data-note="</html>">notes</p>'),
        'textarea.htm': document('<textarea>&lt;html&gt; and </html></textarea><p>end</p>'),
        'table.html': document('<table>' + '<tr><td>日本</td><td>12.5</td></tr>' * 2000 + '</table>'),
        'style.htm': b'<!DoCtYpE HTML><HTML><head><meta http-equiv="Content-Type" content="text/html; charset=UTF-8"><style>i::before{content:"</html>"}</style></head><body><h1>title</h1></body></HTML>',
    }


class Headings(HTMLParser):
    def __init__(self):
        super().__init__()
        self.elements = []

    def handle_starttag(self, tag, attrs):
        self.elements.append(tag)


def make_deleted_fat(root, files):
    disk = root / 'fat.raw'
    with disk.open('wb') as out:
        out.truncate(40 << 20)
    command('mkfs.fat', '-F', '32', '-s', '1', disk)
    for name, data in files.items():
        source = root / name
        source.write_bytes(data)
        command('mcopy', '-i', disk, source, '::/' + name)
        readback = root / ('before-' + name)
        command('mcopy', '-i', disk, '::/' + name, readback)
        check(readback.read_bytes() == data, name)
    for name in files:
        command('mdel', '-i', disk, '::/' + name)
    listing = command('mdir', '-i', disk, '::/').stdout.lower()
    check(all(n.encode() not in listing for n in files), listing)
    return disk


def verify(binary, root, native=False):
    files = samples()
    hashes = {digest(data): name for name, data in files.items()}
    if native:
        disk = make_deleted_fat(root, files)
    else:
        disk = root / 'documents.raw'
        image = bytearray(b'\xad' * ((1 << 20) - 2))
        for name, data in files.items():
            image.extend(data)
            if name == 'corpus.htm':
                image.extend(bytes(16))
        disk.write_bytes(image)
    before = digest(disk.read_bytes())
    for label, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'htm',
                   '--work-dir', root / label, '--json', *flags, timeout=180)
        found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(found) == len(files) and {r['record_sha256'] for r in found} == set(hashes), found)
        ids = [arg for row in found for arg in ('--id', row['id'])]
        rows = run(binary, 'recover', '--session', scan['session'], *ids,
                   '--destination', root / (label + '-out'), '--json')['results']
        check(len(rows) == len(files), rows)
        for row in rows:
            saved = Path(row['path']).read_bytes()
            name = hashes[row['sha256']]
            check(saved == files[name] and digest(saved) == row['sha256'], name)
            parser = Headings()
            parser.feed(saved.decode('utf-8-sig'))
            parser.close()
            check('title' in parser.elements or 'h1' in parser.elements, (name, parser.elements))
        print(label, len(rows), 'actual FAT32 HTML deletions' if native else 'HTML documents',
              'match bytes and SHA-256', flush=True)
    check(digest(disk.read_bytes()) == before, 'source image changed')
    if not native:
        prefix = b'<!doctype html>'
        invalid = [prefix + fragment for fragment in (
            b'<html><p>x</p>', b'<p><b>x</p></b>', b'<p>x\xff</p>', b'<p>x</p>dangling',
            b'<html><script>"</html>"', b'<p>a\0b</p>', b'<p>x</p>\0x' + bytes(17),
            b'<p id=x id=y>x</p>', b'<html><title>title</html>', b'<svg><g/></svg>',
            b'<meta charset=shift_jis><p>x</p>', b'<meta http-equiv="content-type" content="text/html;charset=windows-1252"><p>x</p>',
            b'<html><p>x</p><!-- </html>', b'<p>x</p><!doctype html>',
            b'<i>' * 257 + b'</i>' * 257,
        )] + [b'<!DOCTYPE html PUBLIC "x"><p>x</p>']
        for i, data in enumerate(invalid):
            damaged = root / f'bad-{i}.raw'
            damaged.write_bytes(data)
            scan = run(binary, 'scan', '--source', damaged, '--mode', 'carve', '--extension', 'html',
                       '--work-dir', root / f'bad-{i}', '--json', expected=5)
            rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
            check(rows and all(not row['supported'] for row in rows), (i, rows))

        implicit = root / 'implicit-eof.htm'
        implicit.write_bytes(files['corpus.htm'])
        scan = run(binary, 'scan', '--source', implicit, '--mode', 'carve', '--extension', 'html',
                   '--work-dir', root / 'eof', '--json')
        row, = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(row['record_sha256'] == digest(implicit.read_bytes()), row)
        clean = files['notes.html']
        cross = files['table.html']
        sector = 4096
        source = root / 'coverage.raw'
        source.write_bytes(b'!' * (sector - len(clean)) + clean + bytes(sector)
                           + cross + bytes(sector))
        acquisition = run(binary, 'image', '--source', source, '--destination', root / 'acquired',
                          '--block-size', str(sector), '--json')
        image = Path(acquisition['image'])
        mark_gap(image, sector)
        mark_gap(image, 3 * sector)
        scan = run(binary, 'scan', '--source', image, '--mode', 'carve', '--extension', 'html',
                   '--work-dir', root / 'coverage', '--json', expected=5)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check({r['record_sha256'] for r in rows if r['supported']} == {digest(clean)}, rows)
        check(any(not r['supported'] and r['record'] == 2 * sector for r in rows), rows)
        print(len(invalid), 'invalid documents refused; complete file ending before a gap recovered; crossing file refused', flush=True)
    return {name: digest(data) for name, data in files.items()}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.native)
