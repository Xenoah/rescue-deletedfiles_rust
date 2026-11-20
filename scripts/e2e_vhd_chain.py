"""Import Windows differencing VHDs and recover real NTFS deletions through each overlay."""
import argparse
import filecmp
import hashlib
import json
import lzma
import mmap
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

from e2e import check, run
from e2e_qcow2_backing import checked_import
from e2e_vhd import sign
from e2e_vhdx import CORPUS, digest, native_import, recover, verify_guest

REF = Path(__file__).resolve().parents[1]/'tests/vhd-chain'


def independent_base(image, raw, root):
    comparison = root/'tsk-base.raw'
    with comparison.open('xb') as out:
        subprocess.run(['img_cat', '-i', 'vhd', image], stdout=out, check=True, timeout=180)
    check(filecmp.cmp(raw, comparison, shallow=False), 'Sleuth Kit base read disagrees with Windows')
    comparison.unlink()


def member(path, spec, ref):
    compressed = ref/(path.name+'.xz')
    check(digest(compressed) == spec['xz_sha256'], compressed)
    with lzma.open(compressed) as src, path.open('xb') as out:
        shutil.copyfileobj(src, out, length=1<<20)
    check(path.stat().st_size == spec['bytes'] and digest(path) == spec['sha256'], path)


def compare_driver(raw, archive):
    with lzma.open(archive) as source, raw.open('rb') as result:
        while data := source.read(1 << 20):
            check(data == result.read(len(data)), ('Windows guest byte mismatch', raw, result.tell()))
        check(not result.read(1), 'unexpected guest suffix')


def controls(binary, root, chain, metadata):
    child, middle, base = chain
    final = metadata['members'][-1]
    failed = root/'rejected.raw'
    original = child.read_bytes()
    cases = []
    def reject(label, members, options=(), code=4):
        checked_import(binary, members, failed, options, code, 'vhd')
        cases.append(label)
    reject('missing_chain', [child])
    reject('missing_base', [child, middle], code=2)
    reject('wrong_parent_order', [child, base, middle])
    reject('extra_parent', [middle, base, child])
    reject('repeated_file', [child, middle, middle], code=2)
    bad = root/'alternative.vhd'
    shutil.copyfile(middle, bad)
    reject('selected_basename', [child,bad,base])
    reject('wrong_known_digest', chain, ['--raw-sha256','11'*32])
    reject('wrong_logical_length', chain, ['--raw-bytes',final['guest_bytes']-512])
    header = original[512:1536]
    table, = struct.unpack_from('>Q', header, 16)
    def header_edit(offset, data):
        h=bytearray(header)
        h[offset:offset+len(data)] = data
        sign(h,36)
        return original[:512] + h + original[1536:]
    def edit(offset, data):
        return original[:offset] + data + original[offset+len(data):]
    def integer(value, width=4):
        return value.to_bytes(width, 'big')
    codes = {
        'zero_parent_id': header_edit(40, bytes(16)),
        'different_parent_id': header_edit(40, bytes(range(16))),
        'unknown_locator': header_edit(576, b'????'),
        'empty_locator_table': header_edit(576, bytes(192)),
        'locator_overlaps_bat': header_edit(576+4,integer(65536)),
        'locator_beyond_file': header_edit(576+16, integer(1<<40,8)),
        'locator_too_small': header_edit(576+4, bytes(4)),
        'locator_odd_utf16': header_edit(576+8,integer(3)),
        'locator_reserved': header_edit(576+12,integer(1)),
        'locator_code_repeated': header_edit(600,header[576:580]),
        'unicode_name_terminated_early': header_edit(64,bytes(2)),
        'invalid_utf16_parent_name': header_edit(64,b'\xd8\x00'),
        'locator_basename_disagrees': edit(int.from_bytes(header[616:624],'big'), 'x'.encode('utf-16le')*10),
        'bat_points_into_parent_locator': edit(table,integer(int.from_bytes(header[592:600],'big')//512)),
        'broken_header_checksum': edit(512+36,bytes([original[512+36]^1])),
        'broken_footer_checksum': edit(len(original)-512+64,bytes([original[-512+64]^1])),
    }
    for label, content in codes.items():
        bad.write_bytes(content)
        reject(label, [bad,middle,base])
    # A non-zero VHD parent timestamp binds the selected file's host mtime.
    saved=middle.stat()
    timestamp=123_456_789
    bad.write_bytes(header_edit(56, integer(timestamp)))
    reject('parent_timestamp', [bad,middle,base])
    options=['--raw-sha256', final['guest_sha256']]
    checked_import(binary, [bad,middle,base], failed, options, kind='vhd')
    check(digest(failed) == final['guest_sha256'], 'changed parent mtime with known guest hash')
    failed.unlink()
    os.utime(middle, (saved.st_atime, 946_684_800+timestamp))
    try:
        result=checked_import(binary, [bad,middle,base], failed, [], kind='vhd')
        check(result['sha256'] == final['guest_sha256'], result)
        failed.unlink()
    finally:
        os.utime(middle, ns=(saved.st_atime_ns,saved.st_mtime_ns))
    return sorted(cases)


def check_unflushed(binary, raw, folder, metadata):
    # The application reread its last write before unlink. The captured
    # Microsoft guest still contains the previous 512 bytes in every file.
    offsets = {}
    proof = {}
    with raw.open('rb') as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as guest:
        for name, sha256 in metadata['originals'].items():
            original = (CORPUS/name).read_bytes()
            previous = bytearray(original)
            previous[4096:4608] = bytes(b ^ 0x39 for b in previous[4096:4608])
            check(hashlib.sha256(original).hexdigest() == sha256, name)
            at = guest.find(previous)
            check(at >= 0 and guest.find(previous, at+1) == -1, name)
            offsets[name] = at
            proof[name] = dict(original_sha256=sha256, retained_sha256=hashlib.sha256(previous).hexdigest(),
                               changed_offset=4096, changed_bytes=512)
    for mode in ([], ['--indexed']):
        session = run(binary, 'scan', *mode, '--source', raw, '--work-dir', folder/('scan'+str(len(mode))), '--json')
        rows = run(binary,'results','--session',session['session'],'--json')['candidates']
        check({r['name'] for r in rows} == proof.keys(), rows)
        report = run(binary, 'recover', '--session', session['session'], '--all',
                     '--destination', folder/('out'+str(len(mode))), '--json')
        for row in report['results']:
            path = Path(row['path'])
            retained = proof[path.name]['retained_sha256']
            check(digest(path) == row['sha256'] == retained, path.name)
    for name, at in offsets.items():
        layout = folder/(name+'.layout.json')
        layout.write_text(json.dumps(dict(schema_version=1, sha256=proof[name]['original_sha256'],
                          extents=[dict(offset=at, length=(CORPUS/name).stat().st_size)])),encoding='utf-8')
        target = folder/(name+'.verified')
        run(binary,'recover-extents','--source',raw,'--layout',layout,'--output',target,'--json',expected=4)
        check(not target.exists(), name)
    return proof


def sector_sources(raw, chain, originals):
    # Independently locate each original in the Windows guest and check the
    # MSB-first sector-presence bitmap in the three known Windows VHD layers.
    layers = [path.read_bytes() for path in chain]
    def owner(at):
        for nr, data in enumerate(layers):
            bat, = struct.unpack_from('>Q', data, 512+16)
            block_bytes, = struct.unpack_from('>I', data, 512+32)
            index, within = divmod(at, block_bytes)
            slot, = struct.unpack_from('>I', data, bat+index*4)
            sector = within//512
            if slot != 0xffffffff and data[slot*512+sector//8] & (0x80 >> (sector%8)):
                return chain[nr].name
        raise AssertionError(('missing payload sector',at))
    result = {}
    with raw.open('rb') as f, mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as guest:
        for name in originals:
            b = (CORPUS/name).read_bytes()
            offset = guest.find(b)
            check(offset >= 0 and offset % 512 == 0, name)
            check(owner(offset + 512) == 'child1.vhd' and owner(offset + 4096) == 'child2.vhd', name)
            result[name] = sorted({owner(offset+i) for i in range(0,len(b),512)})
    check(sum(len(v)==3 for v in result.values()) == 4, result)
    return result


def verify(binary, root, tsk=False, windows_driver=False):
    result={}
    for case in ('fixed-child','dynamic-child','dynamic-two-children','dynamic-nonresident','dynamic-flushed'):
        ref=REF/case
        metadata=json.loads((ref/'manifest.json').read_text())
        folder=root/case
        folder.mkdir()
        receipts=json.loads((ref/'driver.json').read_text())
        chain=[]
        for spec in metadata['members']:
            path=folder/spec['filename']
            member(path,spec,ref)
            chain.insert(0,path)
            raw=folder/'logical.raw'
            report=checked_import(binary,chain,raw,['--raw-bytes',spec['guest_bytes'],
                                  '--raw-sha256',spec['guest_sha256']],kind='vhd')
            verify_guest(raw,report,spec)
            oracle = ref/(path.name+'.guest.raw.xz')
            check(digest(oracle) == receipts[oracle.name], oracle)
            compare_driver(raw,oracle)
            if windows_driver:
                sys.path.insert(0,str(REF))
                from capture_driver_raw import read_guest
                current = folder/'current-driver.raw.xz'
                read_guest(path, spec, current)
                compare_driver(raw,current)
                current.unlink()
            if tsk and len(chain) == 1:
                independent_base(path,raw,folder)
            if len(chain) < len(metadata['members']):
                raw.unlink()
        if case == 'dynamic-nonresident':
            proof = dict(unflushed_files=check_unflushed(binary,raw,folder,metadata))
        else:
            hashes = recover(binary,raw,folder/'regular',metadata,folder='deletions')
            check(recover(binary,raw,folder/'indexed',metadata,indexed=True,folder='deletions') == hashes,case)
            proof = dict(originals=hashes)
            if case == 'dynamic-flushed':
                proof['file_sectors_from_vhd'] = sector_sources(raw,chain,hashes)
        raw.unlink()
        if case=='dynamic-two-children':
            result['refusals']=controls(binary,folder,chain,metadata)
        elif case=='fixed-child':
            # The original driver's 2 MiB-block table identifies a whole
            # guest block inherited by the child. Change that byte only in
            # the matching fixed base, preserving its VHD footer, ID and name.
            child,base=chain
            data=child.read_bytes()
            table,=struct.unpack_from('>Q',data,512+16)
            count,bs=struct.unpack_from('>II',data,512+28)
            inherited=next(i for i in range(1,count) if data[table+i*4:table+i*4+4]==b'\xff'*4)
            with base.open('r+b') as f:
                loc=inherited*bs
                f.seek(loc);saved=f.read(1);f.seek(loc);f.write(bytes([saved[0]^0x80]))
            try:
                checked_import(binary,chain,raw,['--raw-sha256',spec['guest_sha256']],4,'vhd')
            finally:
                with base.open('r+b') as f:
                    f.seek(loc);f.write(saved)
            result['changed_same_id_parent_rejected_by_hash']=True
        # Auto-detection and the explicit type agree.
        report=native_import(binary,chain[0],raw,segments=chain[1:])
        verify_guest(raw,report,metadata['members'][-1])
        for item in metadata['members']:
            check(digest(folder/item['filename'])==item['sha256'],item['filename'])
        result[case]=dict(guest_sha256=report['sha256'], members=len(chain), **proof)
        message = ('7 previous versions identified; current SHA-256 rejected for all 7' if 'unflushed_files' in proof
                   else '7 actual NTFS deletions matched all original bytes and SHA-256')
        print(case,len(chain),'Windows VHDs,',message,flush=True)
        shutil.rmtree(folder)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--tsk',action='store_true')
    p.add_argument('--windows-driver',action='store_true')
    p.add_argument('--output',type=Path)
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        report=verify(args.binary.resolve(),Path(tmp),args.tsk,args.windows_driver)
    if args.output:
        args.output.write_text(json.dumps(report,indent=2,sort_keys=True)+'\n',encoding='utf-8')
