"""Cross-check ext4 allocation filtering against real Linux unlinks and byte-identical live controls."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_disks import disk_image
from e2e_hfs_allocation import restore, scan
from e2e_imaging import mark_gap

REFERENCE = Path(__file__).resolve().parents[1]/'tests/ext4-allocation'


def le32(data, offset):
    return struct.unpack_from('<I', data, offset)[0]


def crc(data, seed=0xffffffff):
    # Reflected Castagnoli CRC, raw accumulator as stored by ext4.
    for byte in data:
        seed ^= byte
        for _ in range(8):
            seed = (seed >> 1) ^ (0x82f63b78 if seed & 1 else 0)
    return seed


def independent(image, ref):
    subprocess.run(['e2fsck', '-f', '-n', str(image)], check=True, capture_output=True)
    listing = subprocess.check_output(['fls', '-u', '-r', '-p', '-f', 'ext4', str(image)], text=True)
    paths = {row.split('\t',1)[1] for row in listing.splitlines() if '\t' in row}
    bs = ref['block_size']
    for name, file in ref['originals'].items():
        check('live/'+name in paths and 'deleted/'+name not in paths, listing)
        for k, state in [('deleted_offset','Not Allocated'),('live_offset','Allocated')]:
            for block in range(file[k]//bs, (file[k]+file['bytes']-1)//bs+1):
                raw = subprocess.check_output(['blkstat','-f','ext4',str(image),str(block)],text=True)
                check(state in raw.splitlines(), (name,block,raw))


def controls(binary, root, data, ref):
    bs, files = ref['block_size'], ref['originals']
    sb = data[1024:2048]
    first, group_blocks = le32(sb,20), le32(sb,32)
    seed = le32(sb,624) if le32(sb,96) & 0x2000 else crc(sb[104:120])
    png, wav = files['sample.png'], files['sample.wav']
    group = (png['deleted_offset']//bs-first)//group_blocks
    size = 64 if le32(sb,96) & 0x80 else 32
    desc = (first+1)*bs+size*group
    bitmap = le32(data,desc)*bs
    map_size = group_blocks//8
    def fix_desc(buf):
        buf[desc+30:desc+32] = bytes(2)
        checksum = crc(struct.pack('<I',group) + buf[desc:desc+size], seed)
        struct.pack_into('<H',buf,desc+30,checksum & 0xffff)
    def fix_bitmap(buf):
        checksum = crc(buf[bitmap:bitmap+map_size],seed)
        struct.pack_into('<H',buf,desc+24,checksum & 0xffff)
        if size == 64:
            struct.pack_into('<H',buf,desc+56,checksum >> 16)
        fix_desc(buf)
    def set_state(buf):
        struct.pack_into('<H',buf,1024+58,3)
        struct.pack_into('<I',buf,2044,crc(buf[1024:2044]))
    def uninitialize(buf):
        pos=desc+18
        struct.pack_into('<H',buf,pos,struct.unpack_from('<H',buf,pos)[0] | 2)
        fix_desc(buf)
    def toggle(buf, pos):
        buf[pos] ^= 1
    def switch_bit(buf,index):
        g, bit = divmod(index-first,group_blocks)
        check(g == group,(index,group,g))
        buf[bitmap+bit//8] ^= 1 << (bit%8)
    def bad_count(buf):
        switch_bit(buf, png['deleted_offset']//bs)
        fix_bitmap(buf)
    before_png = {png['live_offset'],png['deleted_offset']}
    def unknown(image, label, expected=0):
        found, rows = scan(binary,image,root/label,extensions='png',expected=expected)
        check({r['record'] for r in rows} == before_png and all(r['allocation_status']=='unknown' for r in rows),rows)
        check(any(w.startswith('ALLOCATION_FALLBACK:') for w in found['warnings']),found)
    image = root/'damaged.raw'
    mutations={'super_checksum':lambda b:toggle(b,2044),'group_checksum':lambda b:toggle(b,desc+30),
               'bitmap_checksum':lambda b:toggle(b,bitmap),'unclean_state':set_state,
               'uninitialized_group':uninitialize,'free_count_disagrees':bad_count}
    for label,edit in mutations.items():
        changed=bytearray(data)
        edit(changed)
        image.write_bytes(changed)
        unknown(image,label)
    image.write_bytes(data)
    acquired=run(binary,'image','--source',image,'--destination',root/'capture',
                 '--block-size',4096,'--json',timeout=180)
    mapped=Path(acquired['image'])
    mark_gap(mapped,bitmap)
    unknown(mapped,'unreadable',expected=5)
    # Reassign only the second WAV block while preserving group free counts, payloads,
    # timestamps and all three source-fingerprint samples. Recompute both ext4 checksums.
    wrapper = root/'reused-gpt.raw'
    ((start,_),)=disk_image(wrapper,[image],sector=4096)
    shift=start*4096
    stat=wrapper.stat()
    found,(row,)=scan(binary,wrapper,root/'prior',extensions='wav')
    check(row['record']==shift+wav['deleted_offset'] and wav['bytes']>bs,row)
    changed=bytearray(data)
    for key in ('deleted_offset','live_offset'):
        switch_bit(changed,wav[key]//bs+1)
    fix_bitmap(changed)
    with wrapper.open('r+b') as out:
        for at,n in [(bitmap,map_size),(desc,size)]:
            out.seek(shift+at)
            out.write(changed[at:at+n])
    os.utime(wrapper,ns=(stat.st_atime_ns,stat.st_mtime_ns))
    after,rows=scan(binary,wrapper,root/'after',extensions='wav')
    check(not rows,rows)
    fingerprints=[json.loads(Path(x['session']).read_text(encoding='utf-8'))['fingerprint'] for x in (found,after)]
    check(fingerprints[0]==fingerprints[1],fingerprints)
    _,all_rows=scan(binary,wrapper,root/'all-reused',scope='all',extensions='wav')
    check({r['record'] for r in all_rows}=={shift+wav[k] for k in ('deleted_offset','live_offset')},all_rows)
    for mode in ([],['--stream']):
        failure=run(binary,'recover','--session',found['session'],'--id',row['id'],
                    '--destination',root/('stale'+str(len(mode))),'--json',*mode,expected=5)
        check(failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(failure),failure)
    # A failed map in group zero must not hide a known allocated block in group one.
    # Put a valid PNG across the boundary in a disposable copy. Group one's first
    # block is its backup superblock and has its allocation bit set independently.
    if group == 1:
        edge = (first + group_blocks) * bs - 20
        data_copy = bytearray(data)
        check(data_copy[bitmap] & 1, 'group one must start with allocated metadata')
        group0_desc = (first+1)*bs
        group0_bitmap = le32(data_copy,group0_desc)*bs
        data_copy[group0_bitmap] ^= 1
        payload = (REFERENCE.parent/'corpus/files/sample.png').read_bytes()
        data_copy[edge:edge+len(payload)] = payload
        image.write_bytes(data_copy)
        _, all_rows = scan(binary,image,root/'mixed-all',scope='all',extensions='png')
        check(edge in {r['record'] for r in all_rows}, 'whole-region baseline missed boundary PNG')
        _, limited = scan(binary,image,root/'mixed',extensions='png')
        check(edge not in {r['record'] for r in limited}, limited)
        mutations['unreadable_then_allocated_group'] = None
    return sorted([*mutations,'unreadable_map','partially_reused_file','stale_recovery'])


def verify(binary, root, *, tsk=False):
    manifest=json.loads((REFERENCE/'manifest.json').read_text())
    manifest.update(json.loads((REFERENCE/'short.json').read_text()))
    result={}
    for name, ref in manifest.items():
        image=root/(name+'.raw')
        data=lzma.decompress((REFERENCE/(name+'.raw.xz')).read_bytes())
        check(hashlib.sha256(data).hexdigest()==ref['disk_sha256'] and len(data)==ref['bytes'],name)
        image.write_bytes(data)
        short = name.endswith('-short')
        flags = le32(data,1024+96)
        check(bool(flags & 0x80) != short and bool(flags & 0x2000) == short, (name,flags))
        files=ref['originals']
        check(len(files)==7 and all(f['deleted_bytes_retained'] for f in files.values()),ref)
        if tsk:
            independent(image,ref)
        _,rows=scan(binary,image,root/(name+'-all'),scope='all')
        check({r['record'] for r in rows}=={f[k] for f in files.values() for k in ('deleted_offset','live_offset')},rows)
        exact=restore(binary,image,files,root/(name+'-plain'))
        check(restore(binary,image,files,root/(name+'-indexed'),indexed=True)==exact,name)
        wrapper=root/(name+'-disk.raw')
        scheme,sector=('mbr',512) if ref['block_size']==1024 else ('gpt',4096)
        ((start,_),)=disk_image(wrapper,[image],sector=sector,scheme=scheme)
        check(restore(binary,wrapper,files,root/(name+'-partition'),shift=start*sector,indexed=True)==exact,name)
        wrapper.unlink()
        print(name,'7 real Linux unlinks byte-exact; all seven live controls excluded',flush=True)
        result[name]=dict(disk_sha256=ref['disk_sha256'],hashes=exact,excluded_live=7,
                          group_descriptor_bytes=32 if short else 64,explicit_checksum_seed=short)
        if ref['block_size']==1024:
            negative=root/(name+'-negative')
            negative.mkdir()
            result['degraded-'+str(32 if short else 64)]=controls(binary,negative,data,ref)
        image.unlink()
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--tsk',action='store_true')
    p.add_argument('--output',type=Path)
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as folder:
        result=verify(args.binary.resolve(),Path(folder),tsk=args.tsk)
    if args.output:
        args.output.write_text(json.dumps(result,indent=2,sort_keys=True)+'\n',encoding='utf-8')
