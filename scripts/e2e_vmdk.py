"""Verify native standalone VMDK import using real NTFS deletions and independent guest hashes."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from e2e import check
from e2e_vhdx import GOLD as WINDOWS, digest, native_import, recover, verify_guest

GOLD = WINDOWS.parent / 'vmdk'


def sparse_negatives(binary, disk, root):
    base = disk.read_bytes()
    gd = struct.unpack_from('<Q', base, 56)[0] * 512
    rgd = struct.unpack_from('<Q', base, 48)[0] * 512
    gt = struct.unpack_from('<I', base, gd)[0] * 512
    rgt = struct.unpack_from('<I', base, rgd)[0] * 512
    length = len(base)
    def u32(n):
        return struct.pack('<I', n)
    def u64(n):
        return struct.pack('<Q', n)
    def replace_descriptor(old, new):
        desc = base[512:21*512].rstrip(b'\0')
        check(old in desc, old)
        return [(512, desc.replace(old, new).ljust(20*512, b'\0'))]
    cases = {
        'dirty': [(72, b'\1')], 'newline-sentinel': [(75, b'!')],
        'bad-grain-size': [(20, u64(3))], 'enormous-capacity': [(12, bytes([255])*8)],
        'oversized-table': [(44, u32(1024))], 'descriptor-overlap': [(28, u64(34))],
        'directory-alias': [(48, u64(gd//512))], 'table-outside': [(gd, u32(length//512+1))],
        'directory-presence': [(rgd, u32(0))], 'table-copies': [(gt, u32(0))],
        'data-alias': [(gt+4, base[gt:gt+4]), (rgt+4, base[rgt:rgt+4])],
        'data-overlaps-metadata': [(gt, u32(21)), (rgt, u32(21))],
        'truncated-data': [(gt, u32(length//512-1)), (rgt, u32(length//512-1))],
        'parent-cid': replace_descriptor(b'parentCID=ffffffff', b'parentCID=abcd1234'),
        'parent-hint': replace_descriptor(b'#DDB', b'parentFileNameHint="parent.vmdk"\n#DDB'),
        'multiple-extents': replace_descriptor(b'#DDB', b'RW 2 SPARSE "a=b.vmdk"\n#DDB'),
        'external-name': replace_descriptor(b'"sparse.vmdk"', b'"../other.vmdk"'),
        'extent-capacity': replace_descriptor(b'RW 196608', b'RW 196609'),
        'wrong-create-type': replace_descriptor(b'monolithicSparse', b'vmfsSparse'),
    }
    bad = root / 'broken.vmdk'
    for label, changes in cases.items():
        modified = bytearray(base)
        for offset, data in changes:
            modified[offset:offset+len(data)] = data
        bad.write_bytes(modified)
        native_import(binary, bad, root / (label + '.raw'), 'vmdk', expected=4)
    print(len(cases), 'damaged or dependent hosted sparse VMDK variants refused', flush=True)


def streamed_negatives(binary, source, root):
    original = source.read_bytes()
    footer = len(original)-1024
    directory = struct.unpack_from('<Q', original, footer+56)[0]*512
    first_table = struct.unpack_from('<I', original, directory)[0]*512
    first = struct.unpack_from('<I', original, first_table)[0]*512
    size = struct.unpack_from('<I', original, first+8)[0]
    last_table = struct.unpack_from('<I', original, directory+8)[0]*512
    entries = struct.unpack_from('<512I', original, last_table)
    last = 512*next(n for n in reversed(entries) if n)
    last_size = struct.unpack_from('<I', original, last+8)[0]
    def byte(at):
        return bytes([original[at]^1])
    def u32(n):
        return struct.pack('<I', n)
    def u64(n):
        return struct.pack('<Q', n)
    cases = {
        'footer-magic': (footer, b'BAD!'), 'footer-marker': (footer-512+12, u32(2)),
        'footer-geometry': (footer+20, u64(256)), 'end-marker': (len(original)-512, u64(1)),
        'directory-marker-kind': (directory-512+12, u32(1)),
        'table-marker-length': (first_table-512, u64(3)),
        'directory-outside': (footer+56, u64(len(original)//512)),
        'grain-lba': (first, u64(17)), 'zero-compressed-bytes': (first+8, u32(0)),
        'oversized-encoded-grain': (first+8, u32(0xffffffff)),
        'trailing-zlib-data': (first+8, u32(size+1)), 'truncated-zlib': (first+8, u32(size-1)),
        'last-grain-adler': (last+12+last_size-1, byte(last+12+last_size-1)),
    }
    for label, (offset, data) in cases.items():
        altered = bytearray(original)
        altered[offset:offset+len(data)] = data
        bad = root/'bad-stream.vmdk'
        bad.write_bytes(altered)
        native_import(binary, bad, root/(label+'.raw'), 'vmdk', expected=4)
    print(len(cases), 'invalid stream markers, addresses, zlib and footer cases refused including the final grain', flush=True)


def verify(binary, root, independent=False):
    fixtures = json.loads((GOLD / 'manifest.json').read_text())
    proof = {}
    for name, item in fixtures.items():
        work = root / name
        work.mkdir()
        source = work / 'renamed.vmdk'
        packed = GOLD / (name + '.vmdk.xz')
        check(digest(packed) == item['packed_sha256'], name)
        with lzma.open(packed) as src, source.open('wb') as dest:
            shutil.copyfileobj(src, dest)
        check(digest(source) == item['vmdk_sha256'], name)
        oracle = json.loads((WINDOWS / item['windows_guest'] / 'manifest.json').read_text())
        if 'suffix_bytes' in item:
            tail = bytes(range(256))*6
            check(len(tail) == item['suffix_bytes'], name)
            oracle['guest_bytes'] += len(tail)
            oracle['guest_sha256'] = item['guest_sha256']
            oracle['guest_mib_sha256'].append(hashlib.sha256(tail).hexdigest())
        else:
            check(oracle['guest_sha256'] == item['guest_sha256'], name)
        raw = work / 'guest.raw'
        result = native_import(binary, source, raw, 'vmdk' if name == 'zero-grain' else None)
        verify_guest(raw, result, oracle)
        proof[name] = recover(binary, raw, work, oracle)
        if independent:
            subprocess.run(['qemu-img', 'compare', '-q', '-f', 'vmdk', '-F', 'raw', source, raw], check=True)
        if name == 'zero-grain':
            with source.open('rb') as image:
                h = image.read(512)
                check(struct.unpack_from('<I', h, 8)[0] & 4, 'explicit zero flag')
                grain = item['zero_guest_offset'] // (512 * struct.unpack_from('<Q', h, 20)[0])
                count = struct.unpack_from('<I', h, 44)[0]
                image.seek(512 * struct.unpack_from('<Q', h, 56)[0] + 4*(grain//count))
                table = struct.unpack('<I', image.read(4))[0]
                image.seek(512*table + (grain%count)*4)
                check(struct.unpack('<I', image.read(4))[0] == 1, 'explicit zero entry')
        if name == 'sparse':
            sparse_negatives(binary, source, work)
        if name == 'footer-only':
            streamed_negatives(binary, source, work)
        check(digest(source) == item['vmdk_sha256'], 'source changed')
        print(name, 'whole Windows guest and seven actual deleted files match bytes and SHA-256', flush=True)
    return proof


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True, type=Path)
    p.add_argument('--independent', action='store_true')
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.independent)
