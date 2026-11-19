"""Import an actual 4Kn Windows VHDX and verify seven NTFS unlinks with 512-byte fixup units."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_vhdx import CORPUS, GOLD, digest, native_import, recover, verify_guest


def find_record(raw, offset, length, record_hash):
    positions = []
    with raw.open('rb') as src:
        src.seek(offset)
        for start in range(offset, offset + length, 1 << 20):
            block = src.read(min(1 << 20, offset + length - start))
            for n in range(0, len(block) - 1023, 1024):
                if block[n:n+4] == b'FILE' and hashlib.sha256(block[n:n+1024]).hexdigest() == record_hash:
                    positions.append(start+n)
    check(len(positions) == 1, positions)
    return positions[0]


def verify(binary, root, independent=False):
    fixture = GOLD / '4kn'
    manifest = json.loads((fixture / 'manifest.json').read_text())
    packed = fixture / 'volume.vhdx.xz'
    check(digest(packed) == manifest['packed_sha256'], 'packed source hash')
    check(manifest['logical_sector'] == manifest['physical_sector'] == 4096, manifest)
    disk = root / '4kn.vhdx'
    with lzma.open(packed) as src, disk.open('wb') as dest:
        shutil.copyfileobj(src, dest)
    check(digest(disk) == manifest['container_sha256'], 'source hash')
    raw = root / 'guest.raw'
    report = native_import(binary, disk, raw)
    verify_guest(raw, report, manifest)
    layout = run(binary, 'inspect', '--source', raw, '--json')['layout']
    check(layout['scheme'] == 'gpt' and layout['sector_size'] == 4096, layout)
    part, = [r for r in layout['regions'] if r['filesystem'] == 'ntfs']
    with raw.open('rb') as src:
        src.seek(part['offset'])
        boot = src.read(512)
        check(struct.unpack_from('<H', boot, 11)[0] == 4096 and boot[64] == 246, boot)
    if independent:
        external = root / 'libvhdi.raw'
        with external.open('wb') as dest:
            subprocess.run(['img_cat', '-i', 'vhd', disk], stdout=dest, check=True)
        check(digest(external) == manifest['guest_sha256'], 'libvhdi logical bytes differ')
        source = ['-f', 'ntfs', '-b', '4096', '-o', str(part['offset'] // 4096)]
        names = subprocess.check_output(['fls', *source, '-dFrp', external], text=True)
        observed = {}
        for line in names.splitlines():
            info, _, path = line.partition('\t')
            if not path.startswith('rescue-vhdx-fixtures/'):
                continue
            name = path.rsplit('/', 1)[-1]
            inode = info.split()[-1].removesuffix(':')
            data = subprocess.check_output(['icat', *source, external, inode])
            check(data == (CORPUS / name).read_bytes(), name)
            observed[name] = hashlib.sha256(data).hexdigest()
        check(observed == manifest['originals'], observed)
    proof = {}
    for mode in ('session', 'index'):
        work = root / mode
        proof[mode] = recover(binary, raw, work, manifest, indexed=mode == 'index')
    session, = (root / 'session/scan').glob('*.rescue-session.json')
    rows = run(binary, 'results', '--session', session, '--json')['candidates']
    target, = [r for r in rows if r['name'] == 'sample.html']
    at = find_record(raw, part['offset'], part['length'], target['record_sha256'])
    with raw.open('rb') as src:
        src.seek(at)
        original = src.read(1024)
    usa, count = struct.unpack_from('<HH', original, 4)
    check(count == 3 and original[510:512] == original[1022:1024] == original[usa:usa+2], 'fixup units')
    for i, pos in enumerate((510, 1022, 6)):
        with raw.open('r+b') as f:
            f.seek(at+pos)
            f.write(bytes([original[pos] ^ 1]))
        try:
            scan = run(binary, 'scan', '--source', raw, '--work-dir', root / f'damage-{i}', '--json', expected=5)
            candidates = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
            valid = {r['name'] for r in candidates if r['supported']}
            check(valid == set(manifest['originals']) - {target['name']}, (i, valid))
        finally:
            with raw.open('r+b') as f:
                f.seek(at+pos)
                f.write(original[pos:pos+1])
    check(digest(raw) == manifest['guest_sha256'] and digest(disk) == manifest['container_sha256'], 'source changed')
    print('Native 4Kn GPT/NTFS: seven real unlinks exact in both scan modes, two torn trailers and wrong fixup count refused', flush=True)
    return proof


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True, type=Path)
    p.add_argument('--independent', action='store_true')
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.independent)
