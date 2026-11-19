"""Restore real deleted files spanning checksummed Btrfs data extents on DUP metadata."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import tempfile

from btrfs_large_samples import samples
from e2e import check, run
from e2e_btrfs_snapshots import sha

GOLD = Path(__file__).resolve().parents[1] / 'tests/btrfs-large-reference'


def flip(path, offset):
    with path.open('r+b') as image:
        image.seek(offset)
        original = image.read(1)
        check(len(original) == 1, 'damage offset outside image')
        image.seek(offset)
        image.write(bytes([original[0] ^ 1]))


def verify(binary, root, fixture_root=GOLD, modes=('zlib', 'zstd')):
    originals = samples()
    proof = {}
    for mode in modes:
        folder = root / mode
        folder.mkdir()
        fixture = fixture_root / mode
        meta = json.loads((fixture / 'manifest.json').read_text())
        expected = {name: hashlib.sha256(data).hexdigest() for name, data in originals.items()}
        check(expected == meta['original_files'], 'independently generated originals')
        disk = folder / 'owned.raw'
        packed = fixture / 'snapshot.raw.xz'
        check(sha(packed) == meta['compressed_sha256'], 'packed fixture')
        with lzma.open(packed, 'rb') as src, disk.open('wb') as dst:
            shutil.copyfileobj(src, dst)
        check(sha(disk) == meta['sha256'] and disk.stat().st_size == meta['raw_bytes'], 'raw fixture')
        args = ['btrfs', '--source', disk, '--current-root', str(meta['subvolume']),
                '--snapshot-root', str(meta['snapshot']), '--json']
        report = run(binary, *args)
        check(report['checksum'] == meta.get('checksum', 'crc32c'), 'filesystem checksum')
        listing = report['candidates']
        check({p for row in listing for p in row['original_paths']} == {'/' + n for n in originals}, listing)
        check(all(not row['inline_extent'] for row in listing), 'regular extents expected')

        def recover(destination):
            result = run(binary, *args, '--all', '--destination', destination)
            restored = {}
            for row in result['results']:
                name = row['original_paths'][0][1:]
                out = Path(row['path'])
                check(out.read_bytes() == originals[name] and sha(out) == expected[name] == row['sha256'], row)
                restored[name] = row['sha256']
            check(restored == expected, restored)
            return restored

        proof[mode] = recover(folder / 'restored')
        first, second = meta['snapshot_leaf_mirrors']
        flip(disk, first)
        recover(folder / 'surviving-mirror')
        flip(disk, second)
        run(binary, *args, expected=4)
        flip(disk, first)
        flip(disk, second)
        flip(disk, meta['first_file_data_sector'])
        damaged = folder / 'damaged-data'
        rows = run(binary, *args, '--all', '--destination', damaged, expected=5)['results']
        failed = [row for row in rows if 'error' in row]
        check(len(failed) == 1 and failed[0]['original_paths'] == ['/log.txt'] and 'checksum' in failed[0]['error'], rows)
        check(len(list(damaged.iterdir())) == 6, 'damaged file was published')
        for row in rows:
            if 'path' in row:
                name = row['original_paths'][0][1:]
                check(Path(row['path']).read_bytes() == originals[name] and row['sha256'] == expected[name], row)
        flip(disk, meta['first_file_data_sector'])
        flip(disk, 65536)
        run(binary, *args, expected=4)
        flip(disk, 65536)
        check(sha(disk) == meta['sha256'], 'damage simulation left fixture changed')
        print(f'{mode}: seven originals match bytes and SHA-256; one lost metadata mirror tolerated, double loss and corrupt data detected', flush=True)
    return proof


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
