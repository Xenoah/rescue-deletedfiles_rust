"""Verify standalone VHD maps, including poisoned clear bitmap sectors and real QEMU images."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_disks import fixed_vhd
from e2e_udif import GOLDEN, digest, file_hash, restore_files
from e2e_udif_codecs import originals

BLOCK = 4096
SECTOR = 512


def sign(header, offset):
    struct.pack_into('>I', header, offset, 0)
    struct.pack_into('>I', header, offset, 0xffffffff ^ sum(header))


def dynamic(logical, fixed_footer):
    footer = bytearray(fixed_footer)
    struct.pack_into('>Q', footer, 16, SECTOR)
    struct.pack_into('>I', footer, 60, 3)
    sign(footer, 64)
    count = (len(logical) + BLOCK - 1) // BLOCK
    header = bytearray(1024)
    struct.pack_into('>8sQQIII', header, 0, b'cxsparse', 2**64 - 1, 1536, 0x10000, count, BLOCK)
    sign(header, 36)
    table = bytearray(b'\xff' * ((count * 4 + 511) // 512 * 512))
    start = len(footer) + len(header) + len(table)
    data = bytearray()
    for i in reversed(range(count)):
        payload = logical[i * BLOCK:(i + 1) * BLOCK].ljust(BLOCK, b'\0')
        if not any(payload):
            continue
        struct.pack_into('>I', table, i * 4, (start + len(data)) // SECTOR)
        bitmap = bytearray(SECTOR)
        host = bytearray(b'\xa5' * BLOCK)
        for j in range(BLOCK // SECTOR):
            low = j * SECTOR
            sector = payload[low:low + SECTOR]
            if any(sector):
                bitmap[j // 8] |= 0x80 >> (j % 8)
                host[low:low + SECTOR] = sector
        data.extend(bitmap + host)
    return footer + header + table + data + footer


def verify(binary, root, native=False):
    if native:
        raw = root / 'reference.raw'
        run(binary, 'import-image', '--source', GOLDEN / 'apm-fat32.dmg', '--output', raw, '--json')
        manifest = json.loads((GOLDEN / 'manifest.json').read_text())
        logical = raw.read_bytes()
        check(digest(logical) == manifest['disk_sha256'], 'reference SHA-256')
        expected = manifest['deleted']
        cases = {}
        for subformat in ('fixed', 'dynamic'):
            disk = root / (subformat + '.vhd')
            subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'vpc', '-o',
                            'force_size=on,subformat=' + subformat, raw, disk], check=True)
            independent = root / (subformat + '-qemu.raw')
            subprocess.run(['qemu-img', 'convert', '-f', 'vpc', '-O', 'raw', disk, independent], check=True)
            check(file_hash(independent) == manifest['disk_sha256'], 'QEMU ' + subformat)
            cases['qemu-' + subformat] = disk
    else:
        payload, expected = originals()
        # Leading guest bytes resemble a QCOW2 image. Entire holes, alternating clear
        # sectors and a partial final cluster exercise bitmap order and host bounds.
        prefix = b'QFI\xfb'.ljust(SECTOR, b'\0') + bytes(BLOCK * 2)
        logical = prefix + payload + bytes(SECTOR) + b'END'.ljust(SECTOR, b'\0')
        raw = root / 'expected.raw'
        raw.write_bytes(logical)
        fixed = root / 'fixed.bin'
        fixed_vhd(raw, fixed)
        dynamic_path = root / 'dynamic.bin'
        dynamic_path.write_bytes(dynamic(logical, fixed.read_bytes()[-512:]))
        cases = {'fixed': fixed, 'dynamic': dynamic_path}

    for label, path in cases.items():
        folder = root / (label + '-check')
        folder.mkdir()
        out = folder / 'guest.raw'
        before = file_hash(path)
        for mode in ('auto', 'explicit'):
            args = ['--image-type', 'vhd'] if mode == 'explicit' else []
            dest = out if mode == 'auto' else folder / 'explicit.raw'
            result = run(binary, 'import-image', '--source', path, '--output', dest, '--json', *args)
            check(result['bytes'] == len(logical) and result['sha256'] == file_hash(dest) == digest(logical), label)
        restore_files(binary, out, folder, expected, metadata=native)
        run(binary, 'import-image', '--source', path, '--output', out, '--json', expected=2)
        check(file_hash(path) == before and file_hash(out) == digest(logical), label)
        print(label, len(expected), 'files and the logical disk match', flush=True)

    if not native:
        base = dynamic_path.read_bytes()
        def change(at, fmt, value, header=None):
            altered = bytearray(base)
            struct.pack_into('>' + fmt, altered, at, value)
            if header is not None:
                region = altered[512:1536]
                sign(region, 36)
                altered[512:1536] = region
            return altered
        damaged = {
            'cut-footer': base[:-1], 'missing-footer': base[:-512],
            'footer-checksum': change(len(base)-448, 'I', 0),
            'backup-checksum': change(64, 'I', 0),
            'header-checksum': change(512+36, 'I', 0),
            'header-version': change(512+24, 'I', 0x20000, True),
            'unaligned-table': change(512+16, 'Q', 1537, True),
            'out-of-range-table': change(512+16, 'Q', 0xfffffffffffffe00, True),
            'zero-table': change(512+28, 'I', 0, True),
            'huge-block': change(512+32, 'I', 0x80000000, True),
            'metadata-overlap': change(1536, 'I', 1),
            'block-outside': change(1536, 'I', 0xfffffffe),
            'duplicate-data': change(1536+4, 'I', struct.unpack_from('>I', base, 1536)[0]),
        }
        for label, position, value, fmt in [('differencing',60,4,'I'), ('wrong-size',48,len(logical)+512,'Q')]:
            broken = bytearray(fixed.read_bytes())
            tail = broken[-512:]
            struct.pack_into('>'+fmt, tail, position, value)
            sign(tail,64)
            broken[-512:] = tail
            damaged[label] = broken
        for label, data in damaged.items():
            path = root / (label+'.vhd')
            dest = root / (label+'.raw')
            path.write_bytes(data)
            run(binary, 'import-image', '--source', path, '--image-type', 'vhd', '--output', dest, '--json', expected=4)
            check(not dest.exists() and path.read_bytes() == data, label)
        print(len(damaged), 'malformed or dependent images refused',flush=True)
    return {key:len(expected) for key in cases}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as folder:
        verify(args.binary.resolve(), Path(folder), args.native)
