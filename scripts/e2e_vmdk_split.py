"""Verify multi-file VMware images against the physical bytes of an actual deleted Windows volume."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check
from e2e_vhdx import GOLD, MIB, digest, native_import, recover, verify_guest

FIXTURE = GOLD.parent / 'vmdk-split'


def describe(kind, extents):
    return ('# Disk DescriptorFile\nversion=1\nCID=47ab12\nparentCID=ffffffff\n'
            f'createType="{kind}"\n\n# Extent description\n' + '\n'.join(extents) + '\n')


def expand(src, target):
    with lzma.open(src) as packed, target.open('wb') as output:
        shutil.copyfileobj(packed, output)


def verify(binary, root, independent=False):
    oracle = json.loads((GOLD / 'fixed/manifest.json').read_text())
    src = root / 'windows.vhdx'
    expand(GOLD / 'fixed/volume.vhdx.xz', src)
    raw = root / 'windows.raw'
    verify_guest(raw, native_import(binary, src, raw), oracle)
    proof = {}

    def trial(name, descriptor, files, qemu=True):
        folder = root / name
        folder.mkdir()
        out = folder / 'decoded.raw'
        before = [digest(p) for p in [descriptor, *files]]
        result = native_import(binary, descriptor, out, 'vmdk' if name == 'flat-split' else None,
                               segments=reversed(files))
        verify_guest(out, result, oracle)
        with raw.open('rb') as reference, out.open('rb') as decoded:
            for block in iter(lambda: reference.read(MIB), b''):
                check(decoded.read(len(block)) == block, name)
        proof[name] = recover(binary, out, folder, oracle)
        if independent and qemu:
            subprocess.run(['qemu-img', 'compare', '-q', '-f', 'vmdk', '-F', 'raw', descriptor, raw], check=True)
        check(before == [digest(p) for p in [descriptor, *files]], 'source changed')
        out.unlink()
        print(name, 'whole guest and seven deleted originals matched in bytes and SHA-256', flush=True)

    desc = root / 'guest.vmdk'
    desc.write_text(describe('monolithicFlat', [f'RW {oracle["guest_bytes"]//512} FLAT "windows.raw" 0']))
    trial('flat', desc, [raw])
    # The fourth of seven consecutive deleted MFT records starts at the split.
    cut = 33289216
    with raw.open('rb') as physical:
        for pos in range(cut-3*1024, cut+4*1024, 1024):
            physical.seek(pos)
            check(physical.read(4) == b'FILE', pos)
        physical.seek(60*MIB)
        check(physical.read(MIB) == bytes(MIB), 'ZERO extent needs actual zero bytes')
        a, b = root / 'front offset.bin', root / 'other extent.bin'
        with a.open('wb') as first, b.open('wb') as second:
            first.write(b'A'*1024)
            physical.seek(0)
            first.write(physical.read(cut))
            first.write(b'B'*512)
            second.write(b'C'*1536)
            second.write(physical.read(60*MIB-cut))
            physical.seek(61*MIB)
            shutil.copyfileobj(physical, first)
            second.write(b'D'*512)
    flat = describe('twoGbMaxExtentFlat', [
        f'RW {cut//512} FLAT "{a.name}" 2',
        f'RW {(60*MIB-cut)//512} FLAT "{b.name}" 3',
        f'RW {MIB//512} ZERO',
        f'RW {(oracle["guest_bytes"]-61*MIB)//512} FLAT "{a.name}" {cut//512+3}'])
    desc.write_text(flat)
    # QEMU's hosted descriptor reader skips ZERO extent declarations.
    trial('flat-split', desc, [a, b], qemu=False)

    manifest = json.loads((FIXTURE / 'manifest.json').read_text())
    check(manifest['guest_sha256'] == oracle['guest_sha256'], manifest)
    check(digest(FIXTURE / 'windows.vmdk') == manifest['descriptor_sha256'], 'descriptor hash')
    sparse = (FIXTURE / 'windows.vmdk').read_text()
    desc.write_text(sparse)
    parts = []
    for item in manifest['parts']:
        part = root / item['name']
        packed = FIXTURE / (part.name + '.xz')
        check(digest(packed) == item['packed_sha256'], item)
        expand(packed, part)
        check(digest(part) == item['sha256'], item)
        parts.append(part)
    trial('sparse-split', desc, parts)

    def refuse(text, files, label):
        desc.write_text(text)
        native_import(binary, desc, root / 'refused.raw', 'vmdk', expected=4, segments=files)
        check(not list(root.glob('*.partial')), label)

    for label, bad, files in [
        ('unsupplied', flat, []), ('missing', flat, [a]), ('unused', flat, [a,b,raw]),
        ('flat-source-overlap', flat.replace(f'"{a.name}" {cut//512+3}', f'"{a.name}" 2'), [a,b]),
        ('flat-truncated', flat.replace(f'"{b.name}" 3', f'"{b.name}" 20'), [a,b]),
        ('unsafe-parent', flat.replace('ffffffff', 'abc12345'), [a,b]),
        ('parent-hint', flat+'parentFileNameHint="disk.vmdk"\n', [a,b]),
        ('duplicate-header', flat+'version=1\n', [a,b]),
        ('bad-cid', flat.replace('47ab12','aaaaz'), [a,b]),
        ('excessive-cid', flat.replace('47ab12','000000000'), [a,b]),
        ('absolute', flat.replace(a.name, '/home/data.bin'), [a,b]),
        ('relative', flat.replace(a.name, '../data.bin'), [a,b]),
        ('windows-path', flat.replace(a.name, 'C:\\data.bin'), [a,b]),
        ('wrong-storage', flat.replace('twoGbMaxExtentFlat','twoGbMaxExtentSparse'), [a,b]),
        ('mixed-kind', flat.replace('twoGbMaxExtentFlat','monolithicFlat'), [a,b]),
        ('out-of-range', flat.replace('RW 2048 ZERO','RW 4194305 ZERO'), [a,b]),
        ('unknown-extent', flat.replace('RW 2048 ZERO','RW 2048 VMFS'), [a,b]),
        ('no-access', flat.replace('RW 2048 ZERO','NOACCESS 2048 ZERO'), [a,b]),
        ('sparse-capacity', sparse.replace('RW 32768','RW 32767'), parts),
        ('sparse-grain-boundary', sparse.replace('# Extent description', '# Extent description\nRW 1 ZERO'), parts),
        ('sparse-duplicate', sparse + f'RW 32768 SPARSE "{parts[0].name}"\n', parts),
    ]:
        refuse(bad, files, label)
    alias = root / 'elsewhere'
    alias.mkdir()
    collision = alias / a.name
    collision.write_bytes(bytes(512))
    refuse(flat, [a,b,collision], 'duplicate basename')
    hardlink = root / 'same-bytes.bin'
    hardlink.hardlink_to(b)
    refuse(flat, [a,b,hardlink], 'same physical file under two names')
    native_import(binary, desc, root / 'duplicate.raw', 'vmdk', expected=2, segments=[a,b,a])
    # Conflict in the final component checks cleanup after earlier components have been written.
    with parts[-1].open('r+b') as component:
        component.seek(512)
        component.write(b'# Disk DescriptorFile')
    refuse(sparse, parts, 'nested descriptor')
    print('25 incomplete, ambiguous, unsafe or damaged descriptor/component combinations refused', flush=True)

    if independent:
        for kind in ('monolithicFlat', 'twoGbMaxExtentFlat', 'twoGbMaxExtentSparse'):
            ref = root / ('qemu-' + kind)
            ref.mkdir()
            descriptor = ref / 'volume.vmdk'
            subprocess.run(['qemu-img','convert','-f','raw','-O','vmdk','-o','subformat='+kind,raw,descriptor],check=True)
            trial(kind, descriptor, sorted(ref.glob('volume-*.vmdk')))
    return proof


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--independent', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as work:
        verify(args.binary.resolve(), Path(work), args.independent)
