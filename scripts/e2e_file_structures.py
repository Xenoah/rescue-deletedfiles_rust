"""Exercise file structures through both scan engines and the public restore path."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import struct
import zlib

from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus'
EXTENSIONS = {'bmp', 'gif', 'sqlite', 'wav', 'webp', 'avi', 'mkv', 'webm', 'ogg', 'gz'}


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
        # Keep the file length and magic intact. Each changed field is part of the
        # format itself, so adjacent readable padding cannot hide this damage.
        damaged = {ext: bytearray(files[ext]) for ext in ('avi', 'mkv', 'webm', 'ogg', 'gz')}
        damaged['avi'][damaged['avi'].index(b'avih') + 4] = 1
        for ext in ('mkv', 'webm'):
            # Unknown-sized Segment as produced by a live Matroska writer.
            pos = damaged[ext].index(bytes.fromhex('18538067')) + 4
            damaged[ext][pos:pos+8] = bytes.fromhex('01ffffffffffffff')
        damaged['ogg'][-1] ^= 1
        damaged['gz'][-8] ^= 1
        for ext, data in damaged.items():
            image_bad = root / (ext + '-damaged.raw')
            image_bad.write_bytes(data + bytes(4096))
            for label, flags in [('session', []), ('index', ['--indexed'])]:
                scan = run(binary, 'scan', '--source', image_bad, '--mode', 'carve', '--extension', ext,
                           '--work-dir', root / (label + ext + '-damaged'), '--json', *flags, expected=5)
                found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
                check(any(c['record'] == 0 and not c['supported'] for c in found)
                      and not any(c['supported'] for c in found), (ext, label, found))

        # Independent RFC 1952 writer with FEXTRA/FNAME/FCOMMENT/FHCRC, plus a
        # fresh member immediately following the first. Neither claims its neighbour.
        payload = bytes(range(256)) * 1024
        header = bytes.fromhex('1f8b081e0000000000ff') + b'\x04\x00testproof\x00comment\x00'
        deflater = zlib.compressobj(wbits=-15)
        member = (header + struct.pack('<H', zlib.crc32(header) & 0xffff)
                  + deflater.compress(payload) + deflater.flush()
                  + struct.pack('<II', zlib.crc32(payload), len(payload)))
        second = gzip.compress(b'next member', mtime=0)
        check(gzip.decompress(member + second) == payload + b'next member', 'gzip oracle')
        gzip_image = root / 'members.raw'
        gzip_image.write_bytes(member + second + b'neighbor')
        scan = run(binary, 'scan', '--source', gzip_image, '--mode', 'carve', '--extension', 'gz',
                   '--work-dir', root / 'members', '--json')
        found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check([(c['record'], c['size'], c['record_sha256']) for c in found] == [
            (0, len(member), hashlib.sha256(member).hexdigest()),
            (len(member), len(second), hashlib.sha256(second).hexdigest())], found)
        check(image.read_bytes() == contents, 'source changed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    verify(parser.parse_args().binary.resolve())
