"""Capture owned Windows differencing VHDX chains and compare Windows driver views after relocation."""
import hashlib
import json
import lzma
from pathlib import Path
import platform
import shutil
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / 'vhd-chain'))
from generate_windows import capture, logical


def main():
    assert platform.system() == 'Windows'
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for name, kind, depth, layered in [
            ('4kn-child', '4kn', 1, False),
            ('4kn-two-children', '4kn', 2, True),
        ]:
            capture(root, name, kind, depth, layered=layered, durable=True, suffix='.vhdx',
                    destination=ROOT, include_guest=True)
            out = ROOT / name
            reference = json.loads((out / 'manifest.json').read_text())
            # Move all source members out of their recorded absolute paths. Replay the
            # explicit siblings in another directory and compare every logical generation.
            moved = root / ('relocated-' + name)
            (root / name).rename(moved)
            for member in reference['members']:
                assert member['logical_sector'] == 4096
                path = moved / member['filename']
                observed = logical(path)
                assert observed['guest_sha256'] == member['guest_sha256']
                assert observed['guest_mib_sha256'] == member['guest_mib_sha256']
                with lzma.open(out / (path.name + '.guest.raw.xz'), 'rb') as stored:
                    assert hashlib.file_digest(stored, 'sha256').hexdigest() == observed['guest_sha256']
            reference['relocated_driver_verified'] = True
            (out / 'manifest.json').write_text(json.dumps(reference, indent=2) + '\n')
            print(name, 'read-only driver replay matched all logical generations', flush=True)


if __name__ == '__main__':
    main()
