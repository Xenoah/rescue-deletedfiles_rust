"""Carve Microsoft and gcab CAB files and verify their archived bytes and original members."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from archive_trials import digest, recover_archives
from e2e import check, run
from e2e_imaging import mark_gap
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / 'tests/cab'


def originals():
    corpus = GOLD.parent / 'corpus/files'
    names = {'doc.rtf': 'sample.rtf', 'logo.png': 'sample.png', 'data.xml': 'sample.xml',
             '記録.xml': 'sample.xml', 'table.csv': 'sample.csv'}
    return {**{n: (corpus / source).read_bytes() for n, source in names.items()},
            'large.bin': bytes(range(251)) * 547, 'void.txt': b''}


def damaged(binary, root, files):
    stored = files['gcab-stored.cab']
    files_at = struct.unpack_from('<I', stored, 16)[0]
    data_at = struct.unpack_from('<I', stored, 36)[0]
    reserved = files['reserved.cab']
    def changed(data, offset, value):
        return data[:offset] + value + data[offset + len(value):]
    def u16(value):
        return struct.pack('<H', value)
    def u32(value):
        return struct.pack('<I', value)
    compressed = files['win-mszip.cab']
    first = struct.unpack_from('<I', compressed, 36)[0]
    bad_signature = bytearray(compressed)
    # Keep the XOR checksum correct even as the MSZIP signature becomes invalid.
    bad_signature[first + 8] ^= 1
    bad_signature[first] ^= 1
    cases = {
        'version': changed(stored, 25, b'\x02'), 'header-reserved': changed(stored, 12, u32(8)),
        'previous-cab': changed(stored, 30, u16(1)), 'next-cab': changed(stored, 30, u16(2)),
        'set-index': changed(stored, 34, u16(1)), 'missing-folder': changed(stored, 26, u16(0)),
        'file-table-overlap': changed(stored, 16, u32(36)), 'missing-members': changed(stored, 28, u16(0)),
        'past-end': changed(stored, 36, u32(len(stored)+1)), 'overlapping-data': changed(stored, 36, u32(files_at)),
        'unknown-quantum': changed(stored, 42, u16(0x1472)), 'unknown-window': changed(stored, 42, u16(0x1f03)),
        'continued-file': changed(stored, files_at+8, u16(0xfffd)), 'oversized-file': changed(stored, files_at, u32(0x7fffffff)),
        'folder-range-gap': changed(stored, files_at+4, u32(1)), 'missing-checksum': changed(stored, data_at, u32(0)),
        'bad-checksum': changed(stored, data_at+9, bytes([stored[data_at+9]^1])),
        'unfinished-block': changed(stored, data_at+6, u16(0)), 'oversized-block': changed(stored, data_at+6, u16(32769)),
        'mszip-signature-valid-checksum': bad_signature, 'truncated': stored[:-1],
        'trailer': changed(stored+b'trailer', 8, u32(len(stored)+7)),
        'nonzero-header-extension': changed(reserved, 40, b'\x01'),
        'nonzero-folder-extension': changed(reserved, 57, b'\x01'),
    }
    for name, data in cases.items():
        path = root / (name+'.raw')
        path.write_bytes(data)
        scan = run(binary, 'scan', '--source', path, '--mode', 'carve', '--extension', 'cab',
                   '--work-dir', root / name, '--json', expected=5)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(rows and all(not row['supported'] for row in rows), name)
    source = root/'coverage.raw'
    source.write_bytes(b'!'*(4096-len(stored)) + stored + bytes(4096-52) + compressed + bytes(4096))
    acquired = run(binary,'image','--source',source,'--block-size','4096','--destination',root/'acquired','--json')
    image = Path(acquired['image'])
    mark_gap(image, 8192)
    scan = run(binary,'scan','--source',image,'--mode','carve','--extension','cab','--work-dir',root/'coverage','--json',expected=5)
    rows = run(binary,'results','--session',scan['session'],'--json')['candidates']
    check([r['record_sha256'] for r in rows if r['supported']] == [digest(stored)], rows)
    print(len(cases), 'CAB failure cases and a missing acquisition range checked', flush=True)


def verify(binary, root, native=False):
    files = {}
    metadata = {}
    for collection in ('windows', 'gcab'):
        path = GOLD / collection
        manifest = json.loads((path / (collection+'.json')).read_text(encoding='utf-8'))
        for name, item in manifest.items():
            data = (path / name).read_bytes()
            check(name not in files and len(data) == item['bytes'] and digest(data) == item['sha256'], name)
            files[name] = data
            metadata[name] = item
    original = originals()
    def inspect(path, name):
        for entry, expected in metadata[name]['members'].items():
            data = subprocess.check_output(['cabextract', '-sp', '-F', entry, path])
            check(data == original[entry] and digest(data) == expected, (name, entry))
    if native:
        disk = make_deleted_ext4(root, files)
        for name in files:
            inspect(root / name, name)
    else:
        disk = root / 'consecutive.raw'
        disk.write_bytes(b'!' * ((1 << 20) - 2) + b''.join(files.values()))
    proof = recover_archives(binary, root, disk, files, 'cab', inspect if native else None)
    if not native:
        damaged(binary, root, files)
    return proof


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.native)
