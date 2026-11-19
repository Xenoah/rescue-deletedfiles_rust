"""Restore five files from independent sparse virtual disks without an external converter."""
import argparse
import hashlib
from pathlib import Path
import struct
import tempfile

from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus' / 'files'
BLOCK = 16384


def vdi(blocks):
    # VDI 1.1 base image. Real data blocks use reversed physical order. The
    # guest also contains both kinds of unmapped blocks and a partial final block.
    allocated = [i for i, data in enumerate(blocks) if any(data)][::-1]
    header = bytearray(1024)
    header[:20] = b'Independent VDI test'
    for offset, value in [(64, 0xbeda107f), (68, 0x10001), (72, 384), (76, 1),
                          (340, 512), (344, 1024), (360, 512), (376, BLOCK),
                          (380, 512), (384, len(blocks)), (388, len(allocated))]:
        struct.pack_into('<I', header, offset, value)
    struct.pack_into('<Q', header, 368, sum(map(len, blocks)))
    for i in range(len(blocks)):
        slot = allocated.index(i) if i in allocated else (0xffffffff - i % 2)
        struct.pack_into('<I', header, 512 + i * 4, slot)
    return bytes(header) + b''.join(b'X' * 512 + blocks[i].ljust(BLOCK, b'\xff') for i in allocated)


def verify(binary, root):
    files = [(CORPUS / ('sample.' + e)).read_bytes() for e in ('avi', 'mkv', 'webm', 'ogg', 'gz')]
    blocks = [data.ljust(BLOCK, b'\0') for data in files]
    blocks[1:1] = [bytes(BLOCK), bytes(BLOCK)]
    blocks.append(bytes(512))
    original = b''.join(blocks)
    disk = root / 'guest.bin'
    container = vdi(blocks)
    disk.write_bytes(container)
    proofs = {}
    for fmt, flags in [('automatic', []), ('explicit', ['--image-type', 'vdi'])]:
        out = root / (fmt + '.raw')
        result = run(binary, 'import-image', '--source', disk, '--output', out, '--json', *flags)
        check(out.read_bytes() == original and result['sha256'] == hashlib.sha256(original).hexdigest()
              and result['bytes'] == len(original), fmt)
        scan = run(binary, 'scan', '--source', out, '--mode', 'carve', '--json',
                   '--work-dir', root / (fmt + '-work'))
        candidates = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(candidates) == len(files), candidates)
        digests = []
        for candidate in candidates:
            row, = run(binary, 'recover', '--session', scan['session'], '--id', candidate['id'],
                       '--destination', root / (fmt + '-out'), '--json')['results']
            data = Path(row['path']).read_bytes()
            check(data in files and hashlib.sha256(data).hexdigest() == row['sha256'], row)
            digests.append(row['sha256'])
        check(sorted(digests) == sorted(hashlib.sha256(b).hexdigest() for b in files), fmt)
        run(binary, 'import-image', '--source', disk, '--output', out, '--json', *flags, expected=2)
        check(out.read_bytes() == original and disk.read_bytes() == container, 'source/collision')
        proofs[fmt] = {'guest_sha256': result['sha256'], 'restored_sha256': sorted(digests)}
        print(fmt, '5 of 5 restored, logical RAW SHA-256', result['sha256'])
    def altered(offset, value):
        data = bytearray(container)
        struct.pack_into('<I', data, offset, value)
        return data
    for name, data, code in [('truncated', container[:-1], 4),
                             ('parent', altered(424, 1), 4),
                             ('differencing', altered(76, 4), 4),
                             ('header-overlap', altered(340, 4), 4),
                             ('invalid-slot', altered(512, 77), 4),
                             ('duplicate-slot', altered(512, 0), 4)]:
        damaged = root / (name + '.vdi')
        out = root / (name + '.raw')
        damaged.write_bytes(data)
        run(binary, 'import-image', '--source', damaged, '--output', out, '--json', expected=code)
        check(not out.exists() and damaged.read_bytes() == data, name)
    return proofs


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(parser.parse_args().binary.resolve(), Path(tmp))
