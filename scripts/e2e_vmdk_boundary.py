"""Verify QEMU-generated split flat and sparse VMDKs with real NTFS unlinks on both sides of 2 GiB."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check
from e2e_vhdx import MIB, digest, native_import, recover
from e2e_vhdx_chunk import relocated_guest


def verify(binary, root):
    boundary = 2 << 30
    source, windows = relocated_guest(binary, root, boundary)
    size = source.stat().st_size
    expected = digest(source)
    proof = {}
    for kind in ('twoGbMaxExtentFlat', 'twoGbMaxExtentSparse'):
        with tempfile.TemporaryDirectory(dir=root) as tmp:
            work = Path(tmp)
            desc = work / 'test.vmdk'
            subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'vmdk', '-o',
                            'subformat='+kind, source, desc], check=True)
            info = json.loads(subprocess.check_output(['qemu-img', 'info', '-f', 'vmdk', '--output=json', desc]))
            details = info['format-specific']['data']
            check(info['virtual-size'] == size and details['create-type'] == kind, info)
            e1, e2 = details['extents']
            check(e1['virtual-size'] == boundary and e2['virtual-size'] == size-boundary, details)
            parts = [work / Path(e['filename']).name for e in (e2, e1)]
            raw = work / 'decoded.raw'
            report = native_import(binary, desc, raw, 'vmdk', segments=parts)
            check(report['bytes'] == size and report['sha256'] == expected == digest(raw), report)
            subprocess.run(['qemu-img', 'compare', '-q', '-f', 'vmdk', '-F', 'raw', desc, raw], check=True)
            with raw.open('rb') as actual, source.open('rb') as original:
                for block in iter(lambda: original.read(4*MIB), b''):
                    check(actual.read(len(block)) == block, 'guest differs')
            restored = recover(binary, raw, work / 'recovered', windows)
            proof[kind] = dict(bytes=size, guest_sha256=expected, before=3, after=4, originals=restored)
            # An intact first component must never be published as a complete guest.
            native_import(binary, desc, work / 'incomplete.raw', 'vmdk', expected=4, segments=parts[1:])
            print(kind, size, 'guest bytes matched and seven original deletions restored across the 2 GiB boundary', flush=True)
    return proof


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True, type=Path)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
