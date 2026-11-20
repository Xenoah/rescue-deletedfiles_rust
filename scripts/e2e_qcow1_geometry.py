"""Read five original QCOW v1 layouts with differing cluster sizes and independent L2 table widths."""
import argparse
import hashlib
from pathlib import Path
import struct
import tempfile
import zlib
from e2e import check
from e2e_android_sparse import originals, carve
from e2e_qcow1 import import_as
from e2e_qcow2_subclusters import command


def build(bits, depth, files):
    cluster, per_table = 1<<bits, 1<<depth
    raw=bytearray(49152)
    for data in files.values():
        raw.extend(bytes(cluster-len(raw)%cluster))
        raw.extend(data)
    raw.extend(bytes((-len(raw))%512))
    disk=bytearray(48)
    nblocks=(len(raw)+cluster-1)//cluster
    ntables=(nblocks+per_table-1)//per_table
    struct.pack_into('>4sIQIIQBBHIQ',disk,0,b'QFI\xfb',1,0,0,0,len(raw),bits,depth,0,0,48)
    disk.extend(bytes(8*ntables))
    def align(n):
        disk.extend(bytes((-len(disk))%n))
    tables={}
    mapped=0
    for index in reversed(range(nblocks)):
        data=raw[index*cluster:(index+1)*cluster].ljust(cluster,b'\0')
        if not any(data):
            continue
        number=index//per_table
        if number not in tables:
            align(cluster)
            tables[number]=len(disk)
            struct.pack_into('>Q',disk,48+number*8,len(disk))
            disk.extend(bytes(8*per_table))
        encoder=zlib.compressobj(wbits=-12)
        packed=encoder.compress(data)+encoder.flush()
        if mapped%2==0 and len(packed)<cluster:
            entry=(1<<63) | (len(packed)<<(63-bits)) | len(disk)
            disk.extend(packed)
        else:
            align(cluster)
            entry=len(disk)
            disk.extend(data)
        struct.pack_into('>Q',disk,tables[number]+8*(index%per_table),entry)
        mapped+=1
    return disk,raw


def verify(binary,root,independent=False):
    files=originals()
    rows={}
    for bits,depth in [(9,6),(9,13),(12,7),(15,6),(16,13)]:
        name=f'{1<<bits}-byte-cluster-{1<<depth}-entries'
        case=root/name
        case.mkdir()
        container,expected=build(bits,depth,files)
        path=case/'disk.qcow'
        path.write_bytes(container)
        if independent:
            qemu=case/'qemu.raw'
            command('qemu-img','convert','-f','qcow','-O','raw',path,qemu)
            check(qemu.read_bytes()==expected,name)
        out=case/'guest.raw'
        result=import_as(binary,path,out,'qcow1')
        check(result['sha256']==hashlib.sha256(expected).hexdigest() and out.read_bytes()==expected,result)
        rows[name]={'guest_sha256':result['sha256'], 'originals':carve(binary,out,case/'files',files,False)}
        check(path.read_bytes()==container,name)
        print(name,'seven files verified, guest bytes exact',flush=True)
    return rows


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--independent',action='store_true')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(),Path(tmp),args.independent)
