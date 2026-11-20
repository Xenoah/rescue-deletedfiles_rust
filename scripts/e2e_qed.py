"""Read QEMU-made QED images and compare their guest disks and recovered original files."""
import argparse
import json
import lzma
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check
from e2e_android_sparse import carve
from e2e_lvm2 import sha
from e2e_lvm2_snapshots import carved
from e2e_qcow2_backing import checked_import
from e2e_vdi_chain import changed
from e2e_vhdx import CORPUS, digest, native_import, recover

GOLD = CORPUS.parents[1] / 'qed'
TESTS = GOLD.parent


def deleted(binary, fs, raw, folder, indexed):
    if fs == 'ntfs':
        proof = json.loads((TESTS/'vhd-chain/dynamic-flushed/manifest.json').read_text(encoding='utf-8'))
        return recover(binary, raw, folder, proof, indexed, folder='deletions')
    if fs == 'ext4':
        proof = json.loads((TESTS/'lvm2-snapshots/exhausted-store/manifest.json').read_text(encoding='utf-8'))
        hashes = proof['logical']['deleted']['matched']
        carved(binary, raw, folder, hashes, False, indexed)
        return hashes
    proof = json.loads((TESTS/'vdi-chain/manifest.json').read_text(encoding='utf-8'))['fat32-zero']
    files = {name: (CORPUS/name).read_bytes() for name in proof['originals']}
    return carve(binary, raw, folder, files, False, indexed)


def layouts(binary, work, native):
    proof = {}
    for name, meta in json.loads((GOLD/'manifest.json').read_text(encoding='utf-8')).items():
        folder = work/name
        folder.mkdir()
        image = folder/'disk.qed'
        image.write_bytes(lzma.decompress((GOLD/(name+'.qed.xz')).read_bytes()))
        original = lzma.decompress((TESTS/meta['reference']).read_bytes())
        check(sha(original) == meta['reference_sha256'], name)
        original += bytes.fromhex(meta['appended_sector_hex'])
        check(len(original) == meta['logical_bytes'] and sha(original) == meta['logical_sha256'], name)
        expected = meta['container_sha256']
        check(digest(image) == expected, name)
        raw = folder/'restored.raw'
        for indexed in (False, True):
            if indexed:
                result = checked_import(binary, [image], raw, ['--raw-bytes', len(original),
                                         '--raw-sha256', meta['logical_sha256']], kind='qed')
            else:
                result = native_import(binary, image, raw)
            check(raw.read_bytes() == original and result['sha256'] == digest(raw) == meta['logical_sha256'], name)
            hashes = deleted(binary, meta['filesystem'], raw, folder/('index' if indexed else 'ordinary'), indexed)
            raw.unlink()
        if native:
            subprocess.run(['qemu-img', 'check', '-q', '-f', 'qed', image], check=True)
            subprocess.run(['qemu-img', 'convert', '-f', 'qed', '-O', 'raw', image, raw], check=True)
            check(raw.read_bytes() == original, name)
            raw.unlink()
        check(digest(image) == expected, name)
        proof[name] = dict(logical_sha256=meta['logical_sha256'], recovered=hashes)
        print(name, len(hashes), 'actual deleted files and the complete QED guest match', flush=True)
    return proof


def controls(binary, work):
    image = work/'qemu10-ext4/disk.qed'
    output = work/'refused.raw'
    data = image.read_bytes()
    word = lambda at: struct.unpack_from('<Q', data, at)[0]
    cluster, n = struct.unpack_from('<II', data, 4)
    l1 = word(40)
    l2 = word(l1)
    stride = cluster*n//8
    valid = [(i,word(l2+i*8)) for i in range(stride) if word(l2+i*8)>1]
    (a,first), (b,second) = valid[:2]
    check(first != second, (first, second))
    bad = [
        ('bad-magic',0,b'QED!'), ('bad-cluster',4,struct.pack('<I',6144)),
        ('bad-table',8,struct.pack('<I',3)), ('empty-header',12,bytes(4)),
        ('overlapping-header',12,struct.pack('<I',65535)), ('dirty',16,struct.pack('<Q',2)),
        ('raw-flag-alone',16,struct.pack('<Q',4)), ('unknown-feature',16,struct.pack('<Q',8)),
        ('autoclear-required',32,struct.pack('<Q',1)), ('l1-in-header',40,bytes(8)),
        ('overflowing-l1',40,struct.pack('<Q',2**64-cluster)),
        ('unaligned-size',48,struct.pack('<Q',word(48)-1)),
        ('impossible-size',48,struct.pack('<Q',2**63)),
        ('table-in-l1',l1,struct.pack('<Q',l1)), ('overlapping-l2',l1+8,struct.pack('<Q',l2+cluster)),
        ('misaligned-l2',l1,struct.pack('<Q',l2+1)),
        ('l1-beyond-guest',l1+8*(stride-1),struct.pack('<Q',l2)),
        ('duplicate-data',l2+b*8,struct.pack('<Q',first)),
        ('data-overlaps-table',l2+a*8,struct.pack('<Q',l2)),
        ('data-outside-file',l2+a*8,struct.pack('<Q',len(data))),
        ('misaligned-data',l2+a*8,struct.pack('<Q',first+2)),
    ]
    refused = []
    for label,offset,patch in bad:
        with changed(image, offset, patch):
            checked_import(binary, [image], output, [], code=4, kind='qed')
        refused.append(label)
    expected = json.loads((GOLD/'manifest.json').read_text(encoding='utf-8'))['qemu10-ext4']['logical_sha256']
    with changed(image, first+91, bytes([data[first+91]^1])):
        checked_import(binary, [image], output, ['--raw-sha256', expected], code=4, kind='qed')
        res = native_import(binary, image, output)
        check(res['sha256'] == digest(output) != expected, res)
        output.unlink()
    refused.append('changed-valid-data-original-digest')
    return sorted(refused)


def trailer(meta):
    n = meta['tail_bytes']
    marker = meta['tail_marker']
    if not marker:
        return b''
    if marker == 'base':
        return b'B' * n
    data = bytearray(b'B' * 66048)
    data[32768:40960] = b'M' * 8192
    if marker == 'middle':
        return bytes(data)
    check(marker == 'youngest', marker)
    data[8192:12288] = bytes(4096)
    return bytes(data) + bytes(n-len(data)-512) + b'Z' * 512


def history(binary, work, native):
    proofs = {}
    for label in ('short-middle', 'windows-raw'):
        case = GOLD/label
        folder = work/label
        folder.mkdir()
        proof = json.loads((case/'manifest.json').read_text(encoding='utf-8'))
        paths, hashes = [], []
        for i, spec in enumerate(proof['members']):
            source = lzma.decompress((TESTS/spec['reference']).read_bytes()) + trailer(spec)
            check(len(source) == spec['logical_bytes'] and sha(source) == spec['logical_sha256'], spec)
            part = folder/spec['filename']
            part.write_bytes(source if part.suffix == '.raw' else lzma.decompress((case/(part.name+'.xz')).read_bytes()))
            check(digest(part) == spec['container_sha256'], part)
            paths.insert(0, part)
            if part.suffix == '.raw':
                hashes.append(digest(part))
                continue
            output = folder/'view.raw'
            actual = checked_import(binary, paths, output, ['--raw-bytes',len(source),'--raw-sha256',sha(source)],kind='qed')
            check(output.read_bytes() == source and digest(output) == actual['sha256'] == spec['logical_sha256'], part)
            hashes.append(actual['sha256'])
            if native:
                reference = folder/'independent.raw'
                subprocess.run(['qemu-img','check','-q','-f','qed',part], check=True)
                subprocess.run(['qemu-img','convert','-f','qed','-O','raw',part,reference], check=True)
                check(reference.read_bytes() == source, (part,reference))
                reference.unlink()
            if i == 2:
                matched = deleted(binary, proof['filesystem'], output, folder/'ordinary', False)
                check(deleted(binary, proof['filesystem'], output, folder/'index', True) == matched, part)
            output.unlink()
        # Auto-detection must inherit the same explicitly supplied members.
        result = native_import(binary, paths[0], output, segments=paths[1:])
        check(output.read_bytes() == source and result['sha256'] == hashes[-1], result)
        output.unlink()
        rejects = {}
        def refuse(name, files, code=4, extra=()):
            checked_import(binary, files, output, list(extra), code=code, kind='qed')
            rejects[name] = True
        refuse('missing_all_parents', paths[:1], 2)
        refuse('missing_base', paths[:2], 2)
        refuse('wrong_order', [paths[0], paths[2], paths[1]])
        refuse('wrong_length', paths, extra=['--raw-bytes', len(source)+512])
        refuse('wrong_hash', paths, extra=['--raw-sha256', 'ab'*32])
        refuse('repeated_input', [*paths,paths[-1]], 2)
        if label == 'short-middle':
            refuse('extra_after_standalone', [paths[-1],paths[0]])
        h = paths[0].read_bytes()
        at, length = struct.unpack_from('<II',h,56)
        for test, offset, patch in [
            ('backing_name_in_header',56,struct.pack('<I',12)),
            ('backing_name_past_header',60,struct.pack('<I',65535)),
            ('stored_path',at,b'/'), ('embedded_nul',at,b'\0'),
            ('non_utf8',at,b'\xff'), ('name_disagrees',at,b'X'),
        ]:
            with changed(paths[0], offset, patch):
                refuse(test, paths)
        if label == 'windows-raw':
            # Without the explicit RAW flag, a non-QED base must never be guessed as RAW.
            with changed(paths[1],16,struct.pack('<Q',1)):
                refuse('untyped_non_qed_parent', paths)
            # Change bytes in a genuinely inherited RAW cluster without changing the parent name.
            with changed(paths[-1], 0, b'QED\0'):
                result = checked_import(binary, paths, output, [], kind='qed')
                with output.open('rb') as f:
                    check(f.read(4)==b'QED\0' and result['sha256'] != hashes[-1], result)
                output.unlink()
                refuse('different_raw_contents_with_original_hash', paths, extra=['--raw-sha256', hashes[-1]])
        check(all(digest(p)==m['container_sha256'] for p,m in zip(paths[::-1],proof['members'])), label)
        proofs[label] = dict(volumes=hashes,recovered=matched,refused=sorted(rejects))
        if native and label == 'short-middle':
            mapping = json.loads(subprocess.check_output(['qemu-img','map','--output=json','-f','qed',paths[0]]))
            points = [(0, 2, False), (8192, 0, True), (32768, 1, False), (262144-512, 0, False)]
            receipts = {}
            for relative, depth, zero in points:
                pos = (16 << 20) + relative
                row, = [r for r in mapping if r['start'] <= pos < r['start'] + r['length']]
                check(row['depth'] == depth and row['zero'] == zero, (pos, row))
                receipts[str(relative)] = dict(depth=depth, zero=zero)
            proofs[label]['qemu_tail_mapping'] = receipts
        print(label, 'complete disk histories verified and',len(matched),'deleted files exact',flush=True)
    return proofs


def verify(binary, work, native=False):
    return dict(layouts=layouts(binary,work,native), refused=controls(binary,work), chains=history(binary,work,native))


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', type=Path, required=True)
    ap.add_argument('--native', action='store_true')
    ap.add_argument('--output', type=Path)
    args = ap.parse_args()
    with tempfile.TemporaryDirectory() as td:
        result=verify(args.binary.resolve(),Path(td),args.native)
    if args.output:
        args.output.write_text(json.dumps(result,sort_keys=True,indent=2)+'\n',encoding='utf-8')
