"""Verify standalone original QCOW v1 images against QEMU, real deletions and corrupt maps."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import random
import struct
import subprocess
import tempfile

from e2e import check
from e2e_android_sparse import originals, carve
from e2e_html import make_deleted_fat
from e2e_netpbm import make_deleted_ext4
from e2e_qcow2_subclusters import command, extract
from e2e_vhdx import GOLD, digest

REF = GOLD.parent/'qcow-v1'
COMPRESSED = 1 << 63


def sha(data):
    return hashlib.sha256(data).hexdigest()


def mappings(data):
    size = struct.unpack_from('>Q',data,24)[0]
    bits, depth = data[32:34]
    cluster, per_table = 1 << bits, 1 << depth
    l1 = struct.unpack_from('>Q',data,40)[0]
    raw, packed, holes, tables = [], [], [], []
    for index in range((size+cluster-1)//cluster):
        ptr = l1 + index//per_table*8
        table = struct.unpack_from('>Q',data,ptr)[0]
        if not table:
            continue
        if not tables or tables[-1] != (ptr,table):
            tables.append((ptr,table))
        pos = table + index%per_table*8
        value = struct.unpack_from('>Q',data,pos)[0]
        if not value:
            holes.append(pos)
        elif value & COMPRESSED:
            packed.append((pos,value & ((1<<(63-bits))-1),(value>>(63-bits))&(cluster-1)))
        else:
            raw.append((pos,value))
    return raw,packed,holes,tables


def capture(raw, folder, compressed):
    image = folder/'source.qcow'
    if compressed:
        command('qemu-img','create','-f','qcow',image,str(raw.stat().st_size))
        # Use the original QCOW driver's compressed writes directly. qemu-img convert -c
        # also issues a zero-byte finalization request that the v1 driver can reject.
        with tempfile.TemporaryDirectory(dir=folder) as scratch, raw.open('rb') as src:
            parts = Path(scratch)
            writes = []
            while data := src.read(4096):
                if any(data):
                    offset = src.tell()-len(data)
                    name = f'block-{offset}'
                    (parts/name).write_bytes(data)
                    writes.extend(['-c',f'write -q -c -s {name} {offset} {len(data)}'])
            for start in range(0,len(writes),256):
                subprocess.run(['qemu-io','-f','qcow',*writes[start:start+256],image.resolve()],
                               cwd=parts,check=True,capture_output=True,timeout=180)
    else:
        command('qemu-img','convert','-f','raw','-O','qcow',raw,image)
    info = json.loads(command('qemu-img','info','--output=json','-f','qcow',image))
    check(info['format']=='qcow' and info['virtual-size']==raw.stat().st_size,info)
    reference = folder/'qemu.raw'
    command('qemu-img','convert','-f','qcow','-O','raw',image,reference)
    check(reference.read_bytes()==raw.read_bytes(), 'QEMU re-expansion differed')
    direct,packed,holes,tables = mappings(image.read_bytes())
    check(direct and holes and bool(packed)==compressed and len(tables)>=2, image)
    return image, {'plain_clusters':len(direct),'deflate_clusters':len(packed),
                   'unallocated_l2_entries':len(holes),'allocated_l2_tables':len(tables)}


def add_trailer(raw):
    # New sectors outside the filesystem force ordinary data, packed streams and a partial last cluster.
    with raw.open('ab') as f:
        f.write(random.Random(128).randbytes(4096))
        f.write(bytes(4<<20))
        f.write(b'Z'*512)


def fixture(root, dest):
    raw = make_deleted_fat(root,originals())
    add_trailer(raw)
    image, layout = capture(raw,root,True)
    dest.mkdir(exist_ok=True,parents=True)
    (dest/'fat32.qcow.xz').write_bytes(lzma.compress(image.read_bytes()))
    manifest = dict(image_sha256=digest(image),raw_sha256=digest(raw),raw_bytes=raw.stat().st_size,
                    layout=layout,originals={n:sha(v) for n,v in originals().items()})
    (dest/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n',encoding='utf-8')


def native(binary,root):
    rows={}
    for fs in ('fat32','ext4','windows-ntfs'):
        with tempfile.TemporaryDirectory(dir=root) as t:
            stage=Path(t)
            manifest=None
            if fs=='fat32':
                raw=make_deleted_fat(stage,originals())
            elif fs=='ext4':
                raw=make_deleted_ext4(stage,originals())
            else:
                manifest=json.loads((GOLD/'fixed/manifest.json').read_text(encoding='utf-8'))
                source=stage/'windows.vhdx'
                raw=stage/'windows.raw'
                source.write_bytes(lzma.decompress((GOLD/'fixed/volume.vhdx.xz').read_bytes()))
                command('qemu-img','convert','-f','vhdx','-O','raw',source,raw)
                check(digest(raw)==manifest['guest_sha256'],'Windows reference disk')
            add_trailer(raw)
            for compress in (False,True):
                label=fs+('-deflate' if compress else '-sparse')
                case=stage/label
                case.mkdir()
                image,layout=capture(raw,case,compress)
                row=extract(binary,image,raw.read_bytes(),case,originals(),manifest)
                row.update(layout)
                rows[label]=row
                print(label,len(row['originals']),'actual unlinks, guest bytes and SHA-256 match QEMU',flush=True)
    return rows


def import_as(binary, source, target, kind, expected=0):
    args=[binary,'import-image','--source',source,'--output',target,'--json']
    if kind:
        args.extend(['--image-type',kind])
    proc=subprocess.run(args,env=dict(os.environ,PATH=''),capture_output=True,text=True,timeout=120)
    check(proc.returncode==expected,(kind,proc.stdout,proc.stderr))
    if expected:
        check(not target.exists(),target)
        return None
    return json.loads(proc.stdout)


def portable(binary,root):
    data=lzma.decompress((REF/'fat32.qcow.xz').read_bytes())
    meta=json.loads((REF/'manifest.json').read_text(encoding='utf-8'))
    source=root/'anonymous.image'
    source.write_bytes(data)
    check(sha(data)==meta['image_sha256'],source)
    for kind in (None,'qcow1','qcow'):
        label=kind or 'auto'
        raw=root/(label+'.raw')
        row=import_as(binary,source,raw,kind)
        check(digest(raw)==row['sha256']==meta['raw_sha256'] and raw.stat().st_size==row['bytes']==meta['raw_bytes'],row)
        if kind is None:
            saved=carve(binary,raw,root/'recovered',originals(),False)
            check(saved==meta['originals'],saved)
        raw.unlink()
    import_as(binary,source,root/'mismatch.raw','qcow2',4)
    direct,packed,holes,tables=mappings(data)
    bits=data[32]
    first,at,size=packed[0]
    def field(offset,value,width):
        return (offset,value.to_bytes(width,'big'))
    def desc(at,length):
        return COMPRESSED | (length<<(63-bits)) | at
    cases={
        'backing-offset':field(8,4096,8),'backing-size':field(16,8,4),'encrypted':field(36,1,4),
        'bad-size':field(24,513,8),'zero-size':field(24,0,8),'too-many-l1':field(24,1<<60,8),
        'cluster-shift':field(32,31,1),'table-shift':field(33,63,1),'reserved-header':field(34,1,2),
        'l1-misaligned':field(40,49,8),'l1-over-header':field(40,0,8),
        'l2-over-header':field(tables[0][0],8,8),'l2-alias':field(tables[1][0],tables[0][1],8),
        'l2-outside':field(tables[0][0],(len(data)+4095)//4096*4096,8),
        'plain-over-table':field(direct[0][0],tables[0][1],8),'plain-alias':field(holes[0],direct[0][1],8),
        'plain-misaligned':field(direct[0][0],direct[0][1]+1,8),
        'compressed-empty':field(first,desc(at,0),8),'compressed-short':field(first,desc(at,size-1),8),
        'compressed-excess':field(first,desc(at,size+1),8),'compressed-over-table':field(first,desc(tables[0][1],size),8),
        'compressed-past-eof':field(first,desc(len(data)-1,size),8),'compressed-damage':(at,b'\xff\xff'),
        'truncated':None,
    }
    for label,patch in cases.items():
        if patch:
            pos,value=patch
            bad=data[:pos]+value+data[pos+len(value):]
        else:
            bad=data[:max(direct[0][1]+4095,at+size)-1]
        source.write_bytes(bad)
        out=root/(label+'.raw')
        import_as(binary,source,out,'qcow1',4)
        check(source.read_bytes()==bad,label)
    print(len(cases),'malformed/unsupported QCOW v1 cases rejected with no published RAW',flush=True)
    return dict(originals=saved,raw_sha256=meta['raw_sha256'],refused=list(cases))


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path)
    parser.add_argument('--native',action='store_true')
    parser.add_argument('--write-reference',type=Path)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as t:
        if args.write_reference:
            fixture(Path(t),args.write_reference)
        else:
            (native if args.native else portable)(args.binary.resolve(),Path(t))
