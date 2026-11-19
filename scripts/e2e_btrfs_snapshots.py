"""Recover actually unlinked Btrfs inline files from CRC32C-checked plain and zlib snapshots."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile

from e2e import check,run

GOLD = Path(__file__).resolve().parents[1]/'tests/btrfs-reference'
CORPUS = GOLD.parent/'corpus/files'


def sha(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()


def crc32c(data):
    table=[]
    for n in range(256):
        for _ in range(8):n=(n >> 1) ^ (0x82f63b78 if n & 1 else 0)
        table.append(n)
    n=0xffffffff
    for b in data:n=(n >> 8) ^ table[(n ^ b) & 255]
    return n ^ 0xffffffff


def verify(binary, root, independent=False):
    proof={}
    for mode in ('plain','zlib'):
        folder=root/mode;folder.mkdir()
        gold=GOLD/mode;meta=json.loads((gold/'manifest.json').read_text())
        packed=gold/'snapshot.raw.xz';disk=folder/'test.raw'
        check(sha(packed)==meta['compressed_sha256'],mode)
        with lzma.open(packed,'rb') as r,disk.open('wb') as w:shutil.copyfileobj(r,w)
        check(sha(disk)==meta['sha256'] and disk.stat().st_size==meta['raw_bytes'],mode)
        base=['btrfs','--source',disk,'--json']
        ids={r['id']:r for r in run(binary,*base)['roots']}
        current=meta['subvolume'];snapshot=meta['snapshot']
        check(ids[snapshot]['readonly'] and ids[snapshot]['parent_uuid']==ids[current]['uuid'],ids)
        args=base+['--current-root',str(current),'--snapshot-root',str(snapshot)]
        found=run(binary,*args)['candidates']
        check({p for row in found for p in row['original_paths']}=={'/'+n for n in meta['original_files']},found)
        dest=folder/'results';rows=run(binary,*args,'--all','--destination',dest)['results']
        recovered={}
        for row in rows:
            name=row['original_paths'][0].removeprefix('/');out=Path(row['path'])
            check(out.read_bytes()==(CORPUS/name).read_bytes() and sha(out)==row['sha256']==meta['original_files'][name],row)
            recovered[name]=row['sha256']
        check(recovered==meta['original_files'],mode)
        selected=found[0]['id']
        one,=run(binary,*args,'--id',str(selected),'--destination',folder/'one')['results']
        check(one['id']==selected and sha(Path(one['path'])) in recovered.values(),one)
        reused=run(binary,*args,'--all','--destination',dest,expected=5)
        check(all('error' in r for r in reused['results']) and len(reused['results'])==len(found),'no clobber')
        run(binary,*args,'--id','1','--destination',folder/'missing',expected=2)
        run(binary,*base,'--current-root',str(snapshot),'--snapshot-root',str(current),expected=2)
        check(not(folder/'missing').exists() and sha(disk)==meta['sha256'],'input preserved')
        proof[mode]=recovered
        print(mode,len(recovered),'historical files matched their original bytes and SHA-256',flush=True)

    # Alter one zlib trailer on a private fixture while retaining a valid metadata CRC.
    ref=meta['snapshot_leaf']
    physical=ref['physical'];nodesize=ref['bytes']
    if independent:
        roots=subprocess.check_output(['btrfs','restore','-l',disk],text=True)
        address=int(re.search(rf'\({snapshot} ROOT_ITEM \d+\) (\d+) level 0',roots)[1])
        mapping=subprocess.check_output(['btrfs-map-logical','-l',str(address),disk],text=True)
        check(address==ref['logical'] and int(re.search(r'mirror 1 logical \d+ physical (\d+) device ',mapping)[1])==physical,'independent chunk and root addresses')
    with disk.open('r+b') as f:
        f.seek(65536+148);check(struct.unpack('<I',f.read(4))[0]==nodesize,'fixture node size')
        f.seek(physical);leaf=bytearray(f.read(nodesize))
        check(struct.unpack_from('<I',leaf)[0]==crc32c(leaf[32:]),'initial metadata CRC32C')
        original=bytes(leaf)
        for i in range(struct.unpack_from('<I',leaf,96)[0]):
            _,kind,_,off,size=struct.unpack_from('<QBQII',leaf,101+i*25)
            if kind==108:
                leaf[101+off+size-1]^=1;break
        else:raise AssertionError('expected inline item')
        f.seek(physical);f.write(leaf)
    run(binary,*args,expected=4)
    with disk.open('r+b') as f:
        struct.pack_into('<I',leaf,0,crc32c(leaf[32:]))
        f.seek(physical);f.write(leaf)
    partial=run(binary,*args,'--all','--destination',folder/'damaged',expected=5)
    check(sum('error' in row for row in partial['results'])==1 and len(list((folder/'damaged').iterdir()))==6,partial)
    with disk.open('r+b') as f:
        f.seek(physical);f.write(original)
        f.seek(65536);old=f.read(1);f.seek(65536);f.write(bytes([old[0]^1]))
    run(binary,*base,expected=4)
    print('Wrong snapshot lineage, live-only ID, existing output, corrupt tree CRC, zlib trailer and superblock refused',flush=True)
    return proof


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--independent',action='store_true',help='cross-check the fixture mapping with btrfs-progs')
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as directory:verify(args.binary.resolve(),Path(directory),args.independent)
