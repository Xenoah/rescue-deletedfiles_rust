"""Convert independent compressed RAW images and restore seven originals from each."""
import argparse
import bz2
import gzip
import json
import lzma
from pathlib import Path
import subprocess
import tempfile
from e2e import check, run
from e2e_udif import GOLDEN, digest, file_hash, restore_files
from e2e_udif_codecs import originals

FIXTURES=GOLDEN.parent/'compressed-raw'


def verify(binary,root,native=False):
    if native:
        original=root/'original.raw'
        run(binary,'import-image','--source',GOLDEN/'apm-fat32.dmg','--output',original,'--json')
        truth=json.loads((GOLDEN/'manifest.json').read_text())
        check(file_hash(original)==truth['disk_sha256'],'independent APM/FAT32 bytes')
        data=original.read_bytes();expected=truth['deleted']
        commands={'gz':['gzip','-c'],'bz2':['bzip2','-c'],'xz':['xz','-c','-1'],
                  'zst':['zstd','-q','--check','-c'],'lz4':['lz4','-q','-BX','-c']}
        streams={k:subprocess.run(cmd,input=data,capture_output=True,check=True).stdout for k,cmd in commands.items()}
    else:
        data,expected=originals()
        manifest=json.loads((FIXTURES/'manifest.json').read_text())
        streams={'gz':gzip.compress(data,mtime=0),'bz2':bz2.compress(data),
                 'xz':lzma.compress(data,format=lzma.FORMAT_XZ,preset=3)}
        for name,meta in manifest['files'].items():
            b=(FIXTURES/name).read_bytes();check(digest(b)==meta['sha256'],name)
            streams[name.rsplit('.',1)[1]]=b
        check(digest(data)==manifest['logical_sha256'],'original hash')
    proof={}
    for i,(ext,b) in enumerate(streams.items()):
        folder=root/ext;folder.mkdir()
        image=folder/('volume.img.'+ext);out=folder/'disk.raw'
        image.write_bytes(b)
        flags=['--image-type','compressed-raw'] if i%2 else []
        report=run(binary,'import-image','--source',image,'--output',out,'--json',*flags)
        check(file_hash(out)==report['sha256']==digest(data) and report['bytes']==len(data) and image.read_bytes()==b,ext)
        restore_files(binary,out,folder,expected,metadata=native)
        proof[ext]=len(expected)
        if not native:
            # A split disk can be compressed as a series of complete same-codec members.
            joined=folder/'joined.img'
            padding=bytes(8) if ext=='xz' else b''
            joined.write_bytes(b+padding+b+padding)
            expanded=folder/'joined.raw'
            restored=run(binary,'import-image','--source',joined,'--output',expanded,'--json')
            check(expanded.read_bytes()==data*2 and restored['sha256']==digest(data*2),ext+' members')
            for label,broken in [('truncated',b[:-4]),('trailing',b+b'EXTRA'),('bad-second-member',b+b[:-4]),
                                 ('corrupt',b[:len(b)//2]+bytes([b[len(b)//2]^255])+b[len(b)//2+1:])]:
                bad=folder/(label+'.img');bad.write_bytes(broken);dest=folder/(label+'.raw')
                run(binary,'import-image','--source',bad,'--image-type','compressed-raw','--output',dest,'--json',expected=4)
                check(not dest.exists(),(ext,label))
        print(ext,len(expected),'original files and entire disk match',flush=True)
    return proof


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--native',action='store_true');a=p.parse_args()
    with tempfile.TemporaryDirectory() as t:verify(a.binary.resolve(),Path(t),a.native)
