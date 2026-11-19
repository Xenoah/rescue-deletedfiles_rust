"""Compare 13 native snapshot deletions under LZO and Zstandard with the kernel-verified originals."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import tempfile

from btrfs_layout_samples import samples
from e2e import check, run
from e2e_btrfs_snapshots import sha

GOLD = Path(__file__).resolve().parents[1] / 'tests/btrfs-lzo-reference'


def verify(binary, root):
    originals = samples()
    proof = {}
    for mode in ('lzo', 'zstd'):
        fixture = GOLD / mode
        meta = json.loads((fixture / 'manifest.json').read_text())
        disk = root / (mode + '.raw')
        packed = fixture / 'snapshot.raw.xz'
        check(sha(packed) == meta['compressed_sha256'], mode)
        with lzma.open(packed, 'rb') as src, disk.open('wb') as dst:
            shutil.copyfileobj(src, dst)
        check(sha(disk) == meta['sha256'] and disk.stat().st_size == meta['raw_bytes'], mode)
        report = run(binary, 'btrfs', '--source', disk, '--current-root', str(meta['subvolume']),
                     '--snapshot-root', str(meta['snapshot']), '--all', '--destination', root / mode, '--json')
        matched = {}
        for row in report['results']:
            name = row['original_paths'][0][1:]
            path = Path(row['path'])
            check(path.read_bytes() == originals[name], name)
            check(row['sha256'] == sha(path) == hashlib.sha256(originals[name]).hexdigest(), name)
            matched[name] = row['sha256']
        check(matched == meta['original_files'] and len(matched) == meta['native_mounted_matches'] == 13, matched)
        check(sha(disk) == meta['sha256'], 'input modified')
        proof[mode] = matched
        print(f'{mode}: all 13 files matched, including inline files, a sliced reflink, preallocated zeros and an empty file', flush=True)
    return proof


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
