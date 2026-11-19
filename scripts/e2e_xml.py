"""Recover declared UTF-8 XML without DTDs, preserving BOMs and root byte boundaries."""
import argparse
import hashlib
from pathlib import Path
import tempfile
import xml.etree.ElementTree as ET
from xml.dom import minidom
from e2e import check, run

DECL = b'<?xml version="1.0" encoding="UTF-8"?>'


def samples():
    elem = ET.Element('{urn:rescue}report', note='Tom & Jerry')
    ET.SubElement(elem, 'name').text = '復元 <A>'
    doc = minidom.Document()
    node = doc.createElement('root')
    doc.appendChild(node)
    node.appendChild(doc.createCDATASection('<fake/> and &ref;'))
    long_text = '日本語 🎵 &amp; '.encode() * 4100
    return {
        'corpus': (Path(__file__).resolve().parents[1] / 'tests/corpus/files/sample.xml').read_bytes(),
        'empty': DECL + b'<empty/>',
        'bom': b'\xef\xbb\xbf' + DECL + b'<root a="1"><empty/></root>',
        'tree': ET.tostring(elem, encoding='utf-8', xml_declaration=True),
        'dom': doc.toxml(encoding='utf-8'),
        'prefix': DECL + b'\r\n<!-- before > --><?mode x?><x y="&quot;">&#x2764;</x>',
        'nested': DECL + '<日本><文 key="&lt;">&amp;</文></日本>'.encode(),
        'long': DECL + b'<root>' + long_text + b'</root>',
    }


def verify(binary, root):
    files = samples()
    for data in files.values():
        ET.fromstring(data)
    # Put the BOM just before a scan window boundary; the inner declaration must not duplicate it.
    image = bytearray(b'\xac' * (1024 * 1024 - 2))
    offsets = {}
    for name in ['bom', *[n for n in files if n != 'bom']]:
        data = files[name]
        offsets[len(image)] = name
        image.extend(data)
        image.extend(b'\n<!-- unassigned trailing bytes -->\0')
    disk = root / 'xml.raw'
    disk.write_bytes(image)
    expected = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
    for mode, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'xml',
                   '--work-dir', root / mode, '--json', *flags)
        found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(found) == len(files) and {c['record'] for c in found} == offsets.keys(), found)
        for c in found:
            name = offsets[c['record']]
            check(c['supported'] and c['size'] == len(files[name]) and c['record_sha256'] == expected[name], (name, c))
            row, = run(binary, 'recover', '--session', scan['session'], '--id', c['id'],
                       '--destination', root / (mode + '-files'), '--json')['results']
            saved = Path(row['path']).read_bytes()
            check(saved == files[name] and row['sha256'] == expected[name], row)
            ET.fromstring(saved)
        print(mode, len(found), 'XML documents byte-exact', flush=True)
    negatives = [DECL + part for part in (
        b'<a><b></a>', b'<x a="1" a="2"/>', b'<unbound:x/>', b'<0bad/>',
        b'<x>&unknown;</x>', b'<x>&#0;</x>', b'<x>\xff</x>', b'<!--bad--data--><x/>',
        b'<!DOCTYPE x SYSTEM "file:///no-such-file"><x/>', b'<!DOCTYPE x><x/>',
        b'<root>' + b'<e>' * 256 + b'</e>' * 256 + b'</root>', b'<root/>'.replace(b'/>', b'>'),
    )]
    negatives += [b'<?xml version="1.0" encoding="ISO-8859-1"?><x/>']
    for i, data in enumerate(negatives):
        invalid = root / f'bad-{i}.xml'
        invalid.write_bytes(data)
        scan = run(binary, 'scan', '--source', invalid, '--mode', 'carve', '--extension', 'xml',
                   '--work-dir', root / f'bad-{i}', '--json', expected=5)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(rows and all(not c['supported'] for c in rows), (i, rows))
    check(disk.read_bytes() == image, 'source changed')
    return expected


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    with tempfile.TemporaryDirectory() as directory:
        verify(p.parse_args().binary.resolve(), Path(directory))
