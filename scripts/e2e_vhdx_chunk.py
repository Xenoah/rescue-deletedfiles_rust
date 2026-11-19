"""Place seven actual deleted Windows NTFS records across the first 4 GiB VHDX BAT chunk boundary."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import uuid

from e2e import check, run
from e2e_vhdx import GOLD, MIB, digest, native_import, recover
from e2e_vhdx_4kn import find_record


def relocated_guest(binary, root, boundary):
    fixture = GOLD / 'fixed'
    m = json.loads((fixture / 'manifest.json').read_text())
    base = root / 'windows.vhdx'
    with lzma.open(fixture / 'volume.vhdx.xz') as src, base.open('wb') as dest:
        shutil.copyfileobj(src, dest)
    guest = root / 'original.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'vhdx', '-O', 'raw', base, guest], check=True)
    check(digest(guest) == m['guest_sha256'], 'QEMU guest differs from Windows')
    part, = run(binary, 'inspect', '--source', guest, '--json')['layout']['regions']
    scan = run(binary, 'scan', '--source', guest, '--work-dir', root / 'original-scan', '--json')
    rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
    records = sorted(find_record(guest, part['offset'], part['length'], r['record_sha256']) - part['offset']
                     for r in rows if r['name'] in m['originals'])
    check(len(records) == 7, records)
    # Place the fourth original record at the requested guest boundary.
    start = boundary - records[3]
    check(start % 512 == 0 and sum(start+x < boundary for x in records) == 3, records)
    source = root / 'large.raw'
    end = start + part['length']
    with guest.open('rb') as src, source.open('wb') as dest:
        header = bytearray(512)
        header[450] = 7
        struct.pack_into('<II', header, 454, start//512, part['length']//512)
        header[510:512] = b'\x55\xaa'
        dest.write(header)
        dest.truncate(end)
        src.seek(part['offset'])
        for pos in range(0, part['length'], MIB):
            data = src.read(min(MIB, part['length']-pos))
            if data.count(0) < len(data):
                dest.seek(start+pos)
                dest.write(data)
    return source, m


def verify(binary, root):
    source, m = relocated_guest(binary, root, 4 << 30)
    end = source.stat().st_size
    image = root / 'large.vhdx'
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'vhdx', '-o',
                    'subformat=dynamic,block_size=1048576', source, image], check=True)
    output = root / 'restored.raw'
    report = native_import(binary, image, output)
    check(report['bytes'] == end and report['sha256'] == digest(source) == digest(output), report)
    restored = recover(binary, output, root / 'restored', m)
    with image.open('r+b') as f:
        f.seek(3 << 16)
        table = f.read(65536)
        bat = next(struct.unpack_from('<Q', table, offset+16)[0]
                   for offset in range(16, 16+32*struct.unpack_from('<I', table, 8)[0], 32)
                   if uuid.UUID(bytes_le=table[offset:offset+16]).fields[0] == 0x2dc27766)
        # Entries 0..4095 are payload; entry 4096 is reserved for a sector
        # bitmap; entry 4097 contains the first payload block of chunk two.
        at = bat+4096*8
        f.seek(at)
        check(f.read(8) == bytes(8) and struct.unpack('<Q', f.read(8))[0] & 7 == 6, 'chunk separator')
        f.seek(at)
        f.write(struct.pack('<Q', 6))
    native_import(binary, image, root / 'invalid.raw', expected=4)
    print(end, 'guest bytes and seven original files matched across VHDX BAT chunks; nonzero standalone bitmap refused', flush=True)
    return dict(bytes=end, guest_sha256=report['sha256'], before_boundary=3, after_boundary=4, originals=restored)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True, type=Path)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        verify(args.binary.resolve(), Path(temp))
