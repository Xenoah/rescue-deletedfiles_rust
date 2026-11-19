"""Recover files through Apple Partition Maps and reject inconsistent partition geometry."""
import argparse
from pathlib import Path
import shutil
import struct
import tempfile

from e2e import check, run
from e2e_extended_filesystems import command
from e2e_formats import corpus_load, file_hash, ntfs_image, positive

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus'
EXTENSIONS = {'png', 'webp', 'bz2', 'xml', 'txt', 'ico', 'zip'}


def make_image(path, block, volume):
    data = volume.read_bytes()
    start = 64
    sectors = len(data) // block
    total = start + sectors + 8
    header = bytearray(512)
    struct.pack_into('>2sHI', header, 0, b'ER', block, total)
    with path.open('xb') as out:
        out.truncate(total * block)
        out.write(header)
        entries = [(1, 8, b'Apple', b'Apple_partition_map'),
                   (start, sectors, 'Café'.encode('mac_roman'), b'Windows_NTFS'),
                   (9, start - 9, b'Spare', b'Apple_Free')]
        for i, (first, size, name, kind) in enumerate(entries, 1):
            row = bytearray(512)
            struct.pack_into('>2s2xIII32s32sIII', row, 0, b'PM', len(entries),
                             first, size, name, kind, 0, size, 0x33)
            out.seek(i * block)
            out.write(row)
        out.seek(start * block)
        out.write(data)


def verify(binary, root):
    files = [e for e in corpus_load(CORPUS) if e['extension'] in EXTENSIONS and e['path'].stem == 'sample']
    check(len(files) == 7, files)
    volume = root / 'ntfs.raw'
    truth, _ = ntfs_image(volume, files, 'fragmented')
    for block in (512, 1024, 2048, 4096):
        disk = root / f'apm-{block}.raw'
        make_image(disk, block, volume)
        layout = run(binary, 'inspect', '--source', disk, '--json')['layout']
        part, = layout['regions']
        check(layout['scheme'] == 'apm' and layout['sector_size'] == block, layout)
        check(part['index'] == 2 and part['offset'] == 64 * block and part['name'] == 'Café'
              and part['length'] == volume.stat().st_size and part['filesystem'] == 'ntfs', part)
        positive(binary, disk, {'p2:' + k: v for k, v in truth.items()}, root, f'apm-{block}', [])
    disk = root / 'apm-512.raw'
    original = disk.read_bytes()
    for label, at, replacement in [
        ('unaligned-block', 2, struct.pack('>H', 513)),
        ('truncated-disk', 4, struct.pack('>I', 0xffffffff)),
        ('count-limit', 516, struct.pack('>I', 257)),
        ('bad-signature', 1024, b'PD'),
        ('count-mismatch', 1028, struct.pack('>I', 1)),
        ('overlap', 1032, struct.pack('>I', 7)),
        ('missing-map', 560, b'Apple_Free\0'),
        ('out-of-range', 1036, struct.pack('>I', 0xffffffff)),
        ('short-map', 524, struct.pack('>I', 2)),
    ]:
        altered = root / (label + '.raw')
        source = bytearray(original)
        source[at:at + len(replacement)] = replacement
        altered.write_bytes(source)
        run(binary, 'inspect', '--source', altered, '--json', expected=4)
        check(altered.read_bytes() == source, label)
    return files


def parted_disk(binary, root, files):
    # GNU Parted writes all map entries; dosfstools and mtools create and unlink the files.
    disk = root / 'gnu-parted.raw'
    vol = root / 'dosfstools.raw'
    mib = 1024 * 1024
    for file, size in [(disk, 68 * mib), (vol, 64 * mib)]:
        with file.open('xb') as f:
            f.truncate(size)
    command('parted', '--script', disk, 'mklabel', 'mac', 'mkpart', 'Rescue', 'fat32', '1MiB', '65MiB')
    command('mkfs.fat', '-F', '32', '-s', '1', vol)
    originals = {}
    for entry in files:
        name = 'recovery-test.' + entry['extension']
        command('mcopy', '-i', vol, entry['path'], '::' + name)
        originals[name] = entry
    for name in originals:
        command('mdel', '-i', vol, '::' + name)
    with disk.open('r+b') as out, vol.open('rb') as src:
        out.seek(mib)
        shutil.copyfileobj(src, out)
    before = file_hash(disk)
    layout = run(binary, 'inspect', '--source', disk, '--json')['layout']
    region, = layout['regions']
    check(layout['scheme'] == 'apm' and region['index'] == 2 and region['offset'] == mib
          and region['length'] == 64 * mib and region['filesystem'] == 'fat32', layout)
    for mode in ['session', 'index']:
        args = ['--indexed'] if mode == 'index' else []
        scan = run(binary, 'scan', '--source', disk, '--work-dir', root / mode, '--json', *args)
        rows = run(binary, 'recover', '--session', scan['session'], '--all',
                   '--destination', root / (mode + '-files'), '--json')['results']
        check(len(rows) == len(files), rows)
        matched = set()
        for row in rows:
            recovered = Path(row['path'])
            original = originals[recovered.name]
            check(recovered.read_bytes() == original['data'] and row['sha256'] == original['sha256'], row)
            matched.add(recovered.name)
        check(matched == originals.keys() and file_hash(disk) == before, mode)
        print('GNU Parted,', mode, len(rows), 'real FAT32 deletions byte-exact', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--parted', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        files = verify(args.binary.resolve(), root)
        if args.parted:
            parted_disk(args.binary.resolve(), root, files)
