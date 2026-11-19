"""Carve real 7-Zip archives, preserving compressed bytes and validating every decoded member."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zlib

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap

GOLD = Path(__file__).resolve().parents[1] / 'tests/sevenz'
CORPUS = GOLD.parent / 'corpus/files'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def check_members(path, members):
    executable = shutil.which('7z') or shutil.which('7zz')
    check(executable, '7-Zip is needed for the independent decoder comparison')
    subprocess.run([executable, 't', path], check=True, stdout=subprocess.DEVNULL)
    for name, original in members.items():
        data = subprocess.check_output([executable, 'x', '-so', path, name])
        check(data == ((CORPUS / original).read_bytes() if original else b''), name)


def rejected(binary, root, files):
    packed = files['stored.7z']
    def mutate(source, where, replacement, sign=False):
        buf = bytearray(source)
        buf[where:where+len(replacement)] = replacement
        if sign:
            struct.pack_into('<I', buf, 8, zlib.crc32(buf[12:32]))
        return bytes(buf)
    offset = 32 + struct.unpack_from('<Q', packed, 12)[0]
    cases = {
        'future-version': mutate(packed,6,b'\x01'), 'start-crc': mutate(packed,8,bytes(4)),
        'header-crc': mutate(packed,28,bytes(4),True),
        'offset-overflow': mutate(packed,12,b'\xff'*8,True),
        'size-overflow': mutate(packed,20,b'\xff'*8,True),
        'empty-header': mutate(packed,20,bytes(8),True),
        'stored-member': mutate(packed,32,bytes([packed[32]^1])),
        'header-damaged': mutate(packed,offset,bytes([packed[offset]^1])),
        'truncated': packed[:-1], 'truncated-compressed': files['solid.7z'][:-3],
        'encrypted': (GOLD/'encrypted.unsupported').read_bytes(),
        'unchecked-codec': (GOLD/'ppmd.unsupported').read_bytes(),
    }
    for name, source in cases.items():
        path=root/(name+'.raw')
        path.write_bytes(source)
        scan=run(binary,'scan','--source',path,'--mode','carve','--extension','7z','--work-dir',root/name,'--json',expected=5)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(rows and all(not row['supported'] for row in rows),name)
    clean=files['lzma.7z']
    source=root/'read-map.raw'
    source.write_bytes(b'!'*(4096-len(clean))+clean+bytes(4096-40)+packed+bytes(4096))
    acq=run(binary,'image','--source',source,'--block-size','4096','--destination',root/'acquisition','--json')
    image=Path(acq['image'])
    mark_gap(image,8192)
    scan=run(binary,'scan','--source',image,'--mode','carve','--extension','7z','--work-dir',root/'map','--json',expected=5)
    rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
    check([row['record_sha256'] for row in rows if row['supported']]==[digest(clean)], rows)
    print(len(cases),'damaged or unsupported archives refused; an unread member range invalidated only the crossing archive',flush=True)


def verify(binary, root, native=False):
    meta=json.loads((GOLD/'manifest.json').read_text(encoding='utf-8'))
    files={name:(GOLD/name).read_bytes() for name in meta}
    hashes={digest(data):name for name,data in files.items()}
    check(len(hashes)==len(meta) and all(meta[name]['sha256']==h and meta[name]['bytes']==len(files[name])
                                          for h,name in hashes.items()),meta)
    if native:
        disk=make_deleted_fat(root,files)
        for name,item in meta.items():
            check_members(root/name,item['members'])
    else:
        disk=root/'consecutive.raw'
        disk.write_bytes(b'!'*((1<<20)-3)+b''.join(files.values()))
    before=digest(disk.read_bytes())
    for label,flags in [('session',[]),('index',['--indexed'])]:
        scan=run(binary,'scan','--source',disk,'--mode','carve','--extension','7z','--work-dir',root/label,'--json',*flags,timeout=180)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(rows)==len(files) and {r['record_sha256'] for r in rows}==set(hashes),rows)
        result=run(binary,'recover','--session',scan['session'],*[x for row in rows for x in ('--id',row['id'])],
                   '--destination',root/(label+'-out'),'--json')['results']
        check(len(result)==len(files),result)
        for row in result:
            path=Path(row['path'])
            original=hashes[row['sha256']]
            check(path.read_bytes()==files[original] and digest(path.read_bytes())==row['sha256'],original)
            if native:
                check_members(path,meta[original]['members'])
        print(label,len(rows),'real FAT32 unlinks' if native else '7z files','restored byte-for-byte with matching SHA-256',flush=True)
    check(digest(disk.read_bytes())==before,'source changed')
    if not native:
        rejected(binary,root,files)
    return {name:item['sha256'] for name,item in meta.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',required=True,type=Path)
    p.add_argument('--native',action='store_true')
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(),Path(tmp),args.native)
