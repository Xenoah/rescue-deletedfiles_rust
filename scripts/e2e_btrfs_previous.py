"""Recover seven real Linux Btrfs unlinks from an older transaction with no user snapshot."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import tempfile

from btrfs_large_samples import samples
from e2e import check, run
from e2e_btrfs_extents import flip
from e2e_btrfs_snapshots import crc32c, sha

GOLD = Path(__file__).resolve().parents[1] / 'tests/btrfs-previous-roots'


def verify(binary, root):
    proof = {}
    originals = samples()
    expected = {name: hashlib.sha256(data).hexdigest() for name, data in originals.items()}
    for mode in ('plain', 'zstd'):
        folder = root / mode
        folder.mkdir()
        fixture = GOLD / mode
        meta = json.loads((fixture / 'manifest.json').read_text())
        packed = fixture / 'volume.raw.xz'
        check(meta['snapshots'] == 0 and expected == meta['originals'], 'originals and absence of snapshots')
        check(sha(packed) == meta['compressed_sha256'], 'compressed fixture')
        disk = folder / 'post-deletion.raw'
        with lzma.open(packed, 'rb') as src, disk.open('wb') as dst:
            shutil.copyfileobj(src, dst)
        check(sha(disk) == meta['sha256'], 'source fixture')
        base = ['btrfs', '--source', disk, '--json']
        inventory = run(binary, *base)
        current = meta['current_generation']
        generation = meta['before_unlink_generation']
        check(inventory['current_generation'] == current and generation in inventory['backup_generations'], inventory)
        check(all(not r['readonly'] for r in inventory['roots']), inventory)
        compare = base + ['--current-root', str(meta['subvolume']), '--previous-generation']
        args = compare + [str(generation)]
        listing = run(binary, *args)
        check(listing['evidence'] == 'previous_generation_only_regular_file_paths' and listing['previous_generation'] == generation, listing)
        ids = {r['original_paths'][0][1:]: r['id'] for r in listing['candidates']}
        check(set(ids) == set(expected), ids)
        check(run(binary, *compare, str(current - 1))['candidates'] == [], 'post-unlink generation')
        for absent in (0, current, current + 1):
            run(binary, *compare, str(absent), expected=2)
        run(binary, *args, '--snapshot-root', '257', expected=2)
        run(binary, *args, '--id', '1', '--destination', folder/'absent', expected=2)
        check(not (folder/'absent').exists(), 'unsupported selection was published')

        def recovered(rows):
            restored = {}
            for row in rows:
                if 'path' in row:
                    name = row['original_paths'][0][1:]
                    output = Path(row['path'])
                    check(output.read_bytes() == originals[name] and sha(output) == row['sha256'] == expected[name], row)
                    restored[name] = row['sha256']
            return restored

        dest = folder/'all'
        proof[mode] = recovered(run(binary, *args, '--all', '--destination', dest)['results'])
        check(proof[mode] == expected, 'all seven originals')
        one = run(binary, *args, '--id', str(ids['bundle.zip']), '--destination', folder/'one')
        check(recovered(one['results']) == {'bundle.zip': expected['bundle.zip']}, one)
        refused = run(binary, *args, '--all', '--destination', dest, expected=5)['results']
        check(len(refused) == 7 and all('error' in row for row in refused), refused)

        # The independently dumped generation-7 extent and chunk trees locate log.txt here.
        flip(disk, 13631488)
        damaged = folder/'damaged'
        rows = run(binary, *args, '--all', '--destination', damaged, expected=5)['results']
        rejected = [r for r in rows if 'error' in r]
        check(len(rejected) == 1 and rejected[0]['original_paths'] == ['/log.txt'] and 'checksum' in rejected[0]['error'], rows)
        check(len(list(damaged.iterdir())) == 6 and set(recovered(rows)) == set(expected) - {'log.txt'}, rows)
        flip(disk, 13631488)
        with disk.open('r+b') as source:
            source.seek(65536)
            pristine = source.read(4096)
            offsets = {struct.unpack_from('<Q', pristine, p + 8)[0]: p for p in range(2859, 2859 + 4*168, 168)}
            selected, recent = offsets[generation], offsets[current]
            for tree in (0, 1, 5):
                # Substitute a valid, but different-generation tree; repair the superblock's CRC.
                sb = bytearray(pristine)
                index = selected + tree * 16
                sb[index:index + 8] = pristine[recent + tree*16:recent + tree*16 + 8]
                struct.pack_into('<I', sb, 0, crc32c(sb[32:]))
                source.seek(65536)
                source.write(sb)
                source.flush()
                run(binary, *args, '--all', '--destination', folder/'stale', expected=4)
                check(not (folder/'stale').exists(), 'stale tree accepted')
            source.seek(65536)
            source.write(pristine)
        check(sha(disk) == meta['sha256'], 'source changed')
        print(mode, 'seven snapshot-free kernel unlinks match original bytes and SHA-256; stale roots and overwritten sectors refused', flush=True)
    return proof


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
