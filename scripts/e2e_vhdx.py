"""Import Windows fixed and expandable VHDX without external readers and recover real NTFS deletions."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import uuid

from e2e import check, run
from e2e_btrfs_snapshots import crc32c

GOLD = Path(__file__).resolve().parents[1] / 'tests/windows-vhdx'
CORPUS = GOLD.parent / 'corpus/files'
MIB = 1 << 20


def digest(path):
    with path.open('rb') as src:
        return hashlib.file_digest(src, 'sha256').hexdigest()


def native_import(binary, src, out, image_type=None, expected=0):
    args = [binary, 'import-image', '--source', src, '--output', out, '--json']
    if image_type:
        args += ['--image-type', image_type]
    p = subprocess.run(list(map(str, args)), env=dict(os.environ, PATH=''), capture_output=True,
                       text=True, encoding='utf-8', timeout=180)
    check(p.returncode == expected, (p.returncode, p.stdout, p.stderr))
    if expected:
        check(not out.exists(), 'partial output after refusal')
        return None
    return json.loads(p.stdout)


def recover(binary, raw, root, manifest, indexed=False):
    scan = run(binary, 'scan', '--source', raw, '--work-dir', root / 'scan', '--json',
               *(['--indexed'] if indexed else []), timeout=180)
    rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
    rows = [r for r in rows if r['name'] in manifest['originals']]
    check({r['name'] for r in rows} == set(manifest['originals']) and len(rows) == 7, rows)
    check(all(r['supported'] and 'rescue-vhdx-fixtures/' in r['original_path'] for r in rows), rows)
    ids = [arg for r in rows for arg in ('--id', r['id'])]
    outputs = run(binary, 'recover', '--session', scan['session'], *ids, '--destination', root / 'files',
                  '--json')['results']
    actual = {}
    for row in outputs:
        path = Path(row['path'])
        name = path.name
        check(path.read_bytes() == (CORPUS / name).read_bytes(), name)
        check(row['sha256'] == digest(path) == manifest['originals'][name], row)
        actual[name] = row['sha256']
    check(actual == manifest['originals'], actual)
    return actual


def verify_guest(raw, report, manifest):
    check(raw.stat().st_size == report['bytes'] == manifest['guest_bytes'], report)
    check(digest(raw) == report['sha256'] == manifest['guest_sha256'], report)
    with raw.open('rb') as src:
        blocks = [hashlib.sha256(src.read(MIB)).hexdigest() for _ in manifest['guest_mib_sha256']]
    check(blocks == manifest['guest_mib_sha256'], 'Windows physical-disk blocks differ')


def signed(blob, at, replacement):
    data = bytearray(blob)
    data[at:at+len(replacement)] = replacement
    data[4:8] = bytes(4)
    struct.pack_into('<I', data, 4, crc32c(data))
    return bytes(data)


def damage_cases(binary, disk, root, manifest):
    # Change the disposable container in place; every trial restores all touched bytes.
    with disk.open('rb') as src:
        def read(offset, size):
            src.seek(offset)
            return src.read(size)
        h1, h2 = (read(i << 16, 4096) for i in (1, 2))
        table = read(3 << 16, 65536)
        regions = {}
        for i in range(struct.unpack_from('<I', table, 8)[0]):
            entry = 16+32*i
            uid = uuid.UUID(bytes_le=table[entry:entry+16])
            regions[uid.fields[0]] = struct.unpack_from('<Q', table, entry+16)[0]
        bat = regions[0x2dc27766]
        meta = regions[0x8b7ca206]
        mt = read(meta, 65536)
        items = {}
        for i in range(struct.unpack_from('<H', mt, 10)[0]):
            e = 32+32*i
            uid = uuid.UUID(bytes_le=mt[e:e+16])
            items[uid.fields[0]] = (meta+e, meta+struct.unpack_from('<I', mt, e+16)[0])
        param_entry, params = items[0xcaa16737]
        size_entry, size = items[0x2fa54224]
        _, logical = items[0x8141bf1d]
        first_slot = read(bat, 8)
    h1_at, h2_at, r1, r2 = [i << 16 for i in (1, 2, 3, 4)]
    active_at, active = max(((h1_at, h1), (h2_at, h2)), key=lambda item: struct.unpack_from('<Q', item[1], 8)[0])
    def regions_both(at, value):
        copy = signed(table, at, value)
        return [(r1, copy), (r2, copy)]
    def u32(n):
        return struct.pack('<I', n)
    def u64(n):
        return struct.pack('<Q', n)
    negatives = {
        'both-header-crc': [(h1_at+4, bytes(4)), (h2_at+4, bytes(4))],
        'both-region-crc': [(r1+4, bytes(4)), (r2+4, bytes(4))],
        'conflicting-header-sequence': [(h1_at, signed(h1, 8, h2[8:16])),
                                        (h2_at, signed(h2, 16, bytes([91])*16))],
        'dirty-active-log': [(active_at, signed(active, 48, bytes([73])*16))],
        'unknown-header-version': [(active_at, signed(active, 66, b'\x02\x00'))],
        'log-overlaps-metadata': [(active_at, signed(active, 72, u64(meta)))],
        'region-copy-disagrees': [(r1, signed(table, 16+28, u32(0)))],
        'unknown-required-region': regions_both(16, bytes([82])*16),
        'region-overlaps-header': regions_both(16+16, u64(0)),
        'region-outside-file': regions_both(16+16, u64(1 << 40)),
        'dependent-parent': [(params+4, u32(2))],
        'missing-sector-size': [(logical, u32(0))],
        'bad-block-size': [(params, u32(3*MIB))],
        'zero-size': [(size, u64(0))],
        'unknown-required-metadata': [(param_entry, bytes([44])*16)],
        'metadata-overlap': [(size_entry+16, u32(params-meta))],
        'payload-overlaps-BAT': [(bat, u64(bat | 6))],
        'payload-alias': [(bat+8, first_slot)],
        'unreadable-undefined-block': [(bat, u64(1))],
        'discarded-block': [(bat, u64(3))],
        'partial-without-parent': [(bat, u64(7))],
        'reserved-bat-bits': [(bat, u64(14))],
        'physical-extent-outside-file': [(bat, u64(1 << 40 | 6))],
    }
    cases = [('redundant-copies', [(active_at+4, bytes(4)), (r1+4, bytes(4))])] + list(negatives.items())
    for name, changes in cases:
        original = []
        with disk.open('r+b') as f:
            for at, data in changes:
                f.seek(at)
                original.append((at, f.read(len(data))))
                f.seek(at)
                f.write(data)
        try:
            out = root / (name + '.raw')
            if name in negatives:
                native_import(binary, disk, out, 'vhdx', expected=4)
            else:
                report = native_import(binary, disk, out, 'vhd')
                verify_guest(out, report, manifest)
                out.unlink()
        finally:
            with disk.open('r+b') as f:
                for at, data in reversed(original):
                    f.seek(at)
                    f.write(data)
    print(len(negatives), 'damaged, ambiguous or dependent VHDX containers refused; redundant-copy recovery verified', flush=True)


def verify(binary, root, native=False):
    proof = {}
    for kind in ('fixed', 'expandable'):
        folder = root / kind
        folder.mkdir()
        detail = GOLD / kind
        manifest = json.loads((detail / 'manifest.json').read_text())
        packed = detail / 'volume.vhdx.xz'
        check(digest(packed) == manifest['packed_sha256'], kind)
        disk = folder / 'windows.vhdx'
        with lzma.open(packed) as src, disk.open('wb') as dest:
            shutil.copyfileobj(src, dest)
        check(digest(disk) == manifest['container_sha256'], kind)
        raw = folder / 'guest.raw'
        report = native_import(binary, disk, raw, 'vhdx' if kind == 'expandable' else None)
        verify_guest(raw, report, manifest)
        proof[kind] = recover(binary, raw, folder, manifest)
        print(kind, 'Windows guest and seven real NTFS deletions byte-exact', flush=True)
        if kind == 'expandable' and not native:
            damage_cases(binary, disk, folder, manifest)
        if native:
            other = folder / 'converted.vhdx'
            variant = 'dynamic' if kind == 'expandable' else 'fixed'
            subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'vhdx', '-o',
                            'subformat=' + variant + ',block_size=1048576', raw, other], check=True)
            decoded = folder / 'qemu.raw'
            result = native_import(binary, other, decoded)
            verify_guest(decoded, result, manifest)
            proof['qemu-' + variant] = recover(binary, decoded, folder / 'qemu', manifest)
            print(variant, 'QEMU guest and seven real Windows deletions byte-exact', flush=True)
        check(digest(disk) == manifest['container_sha256'], 'source changed')
    return proof


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True, type=Path)
    p.add_argument('--native', action='store_true')
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        verify(args.binary.resolve(), Path(temp), args.native)
