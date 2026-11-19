"""Verify seven genuine deletions on each independently created XXH64, SHA-256 and BLAKE2b Btrfs volume."""
import argparse
from pathlib import Path
import tempfile

from e2e_btrfs_extents import verify as verify_extents

GOLD = Path(__file__).resolve().parents[1] / 'tests/btrfs-checksum-reference'


def verify(binary, root):
    return verify_extents(binary, root, GOLD, ('xxhash', 'sha256', 'blake2'))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
