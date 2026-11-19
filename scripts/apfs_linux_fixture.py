"""Create and independently inspect a disposable APFS snapshot with seven real unlinks."""
import hashlib
import json
import lzma
from pathlib import Path
import re
import subprocess
import sys
import tempfile

def invoke(*args):
    return subprocess.run(list(map(str,args)),check=True,capture_output=True,timeout=60).stdout

def main(tools, out):
    out.mkdir(parents=True,exist_ok=True)
    corpus=Path(__file__).resolve().parents[1]/'tests/corpus/files'
    with tempfile.TemporaryDirectory() as tmp:
        root=Path(tmp);raw=root/'owned.img';mount=root/'mnt';mount.mkdir()
        with raw.open('wb') as f:f.truncate(160*1024*1024)
        device=invoke('sudo','losetup','--show','--find',raw).decode().strip()
        assert re.fullmatch(r'/dev/loop[0-9]+',device)
        try:
            invoke('sudo',tools/'mkapfs/mkapfs','-L','RescueFixture',device)
            invoke('sudo','mount','-t','apfs','-o','readwrite',device,mount)
            try:
                folder=mount/'Rescue-Test';invoke('sudo','mkdir',folder)
                expected={}
                for ext in ('png','xml','txt','zip','mp3','rtf','webp'):
                    source=corpus/('sample.'+ext)
                    invoke('sudo','cp',source,folder/source.name)
                    expected['Rescue-Test/'+source.name]=hashlib.sha256(source.read_bytes()).hexdigest()
                invoke('sudo','cp',corpus/'sample.txt',folder/'survivor.txt')
                invoke('sync')
                invoke('sudo',tools/'apfs-snap/apfs-snap',mount,'RescueFixture')
                for name in expected:invoke('sudo','rm',mount/name)
                invoke('sudo','cp',corpus/'sample.txt',folder/'created-later.txt')
                invoke('sync')
                assert all(not (mount/name).exists() for name in expected)
            finally:invoke('sudo','umount',mount)
            invoke('sudo','mount','-t','apfs','-o','ro,snap=RescueFixture',device,mount)
            try:
                assert all(hashlib.sha256(invoke('sudo','cat',mount/name)).hexdigest()==sha for name,sha in expected.items())
                assert not (folder/'created-later.txt').exists()
            finally:invoke('sudo','umount',mount)
        finally:invoke('sudo','losetup','-d',device)
        pstat=invoke('pstat','-P','apfs',raw).decode()
        block=int(re.search(r'APSB Block Number:\s*(\d+)',pstat)[1])
        args=['-f','apfs','-P','apfs','-B',str(block)]
        fsstat=invoke('fsstat',*args,raw).decode()
        xid=int(re.search(r'\[(\d+)\][^\n]*RescueFixture',fsstat)[1])
        old=invoke('fls','-uFrpl',*args,'-S',str(xid),raw).decode()
        live=invoke('fls','-uFrpl',*args,raw).decode()
        for name,sha in expected.items():
            line=next(row for row in old.splitlines() if ':\t'+name+'\t' in row)
            assert ':\t'+name+'\t' not in live
            inode=line.split(':',1)[0].split()[-1]
            assert hashlib.sha256(invoke('icat',*args,'-S',str(xid),raw,inode)).hexdigest()==sha
        for name in ['survivor.txt','created-later.txt']:
            assert name in live
        assert 'created-later.txt' not in old and 'survivor.txt' in old
        data=raw.read_bytes();packed=lzma.compress(data,preset=4)
        assert len(packed)<5*1024*1024
        (out/'snapshot.raw.xz').write_bytes(packed)
        meta=dict(raw_bytes=len(data),sha256=hashlib.sha256(data).hexdigest(),compressed_sha256=hashlib.sha256(packed).hexdigest(),
                  volume='RescueFixture',sector_offset=0,volume_block=block,snapshot_xid=xid,snapshot='RescueFixture',
                  original_files=expected,survivor='Rescue-Test/survivor.txt',created_later='Rescue-Test/created-later.txt',
                  source='Linux APFS read/write module 297a8d0a, apfsprogs b9c4cb4 and an independently checked Sleuth Kit volume')
        (out/'manifest.json').write_text(json.dumps(meta,indent=2)+'\n')
        print('Seven original files verified using a snapshot mount and independent fls/icat',len(packed),'compressed bytes',flush=True)

if __name__=='__main__':main(Path(sys.argv[1]).resolve(),Path(sys.argv[2]).resolve())
