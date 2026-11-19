"""Independently verify truly unlinked, never-snapshotted files using Btrfs backup tree roots."""
import hashlib
import json
import lzma
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from btrfs_large_samples import samples
from generate_btrfs_large_reference import cmd, sha


def generate(mode):
    originals=samples();hashes={n:hashlib.sha256(v).hexdigest() for n,v in originals.items()}
    folder=Path('previous-fixture')/mode;folder.mkdir(parents=True)
    with tempfile.TemporaryDirectory() as t:
        work=Path(t);disk=work/'owned.raw';mnt=work/'mnt';mnt.mkdir()
        with disk.open('wb') as f:f.truncate(160<<20)
        dev=cmd('sudo','losetup','--find','--show',disk).strip()
        assert re.fullmatch('/dev/loop[0-9]+',dev)
        try:
            cmd('sudo','mkfs.btrfs','-q','-f','-m','dup','-d','single',dev)
            opts='nodev,nosuid,nodiscard'+(',compress-force=zstd' if mode=='zstd' else '')
            cmd('sudo','mount','-t','btrfs','-o',opts,dev,mnt)
            try:
                live=mnt/'Live';cmd('sudo','btrfs','subvolume','create',live);cmd('sudo','chown',f'{os.getuid()}:{os.getgid()}',live)
                rootid=int(cmd('sudo','btrfs','inspect-internal','rootid',live).strip())
                for name,data in originals.items():
                    if name=='sparse.dat':
                        with (live/name).open('wb') as f:
                            f.write(data[:4096]);f.seek(len(data)-4096);f.write(data[-4096:])
                    else:(live/name).write_bytes(data)
                (live/'survivor').write_bytes(b'Persists after deletion.\n'*512)
                cmd('sudo','btrfs','filesystem','sync',mnt)
                assert all((live/n).read_bytes()==v and sha(live/n)==hashes[n] for n,v in originals.items())
                prior=int(re.search(r'^generation\s+(\d+)',cmd('sudo','btrfs','inspect-internal','dump-super',dev),re.M)[1])
                for name in originals:(live/name).unlink()
                assert all(not(live/n).exists() for n in originals)
                assert not cmd('sudo','btrfs','subvolume','list','-s',mnt).strip()
                cmd('sudo','btrfs','filesystem','sync',mnt)
            finally:cmd('sudo','umount',mnt)
        finally:cmd('sudo','losetup','--detach',dev)
        cmd('btrfs','check','--readonly',disk)
        present=work/'present';present.mkdir()
        cmd('btrfs','restore','-r',str(rootid),disk,present)
        assert (present/'survivor').is_file() and all(not(present/n).exists() for n in originals)
        dump=cmd('btrfs','inspect-internal','dump-super','-f',disk)
        roots=[(int(gen),int(addr),int(level)) for addr,gen,level in re.findall(r'backup_tree_root:\s*(\d+)\s+gen:\s*(\d+)\s+level:\s*(\d+)',dump)]
        current=int(re.search(r'^generation\s+(\d+)',dump,re.M)[1])
        verified=[]
        for gen,addr,level in sorted(set(roots),reverse=True):
            if gen>=current or not addr:continue
            out=work/f'previous-{gen}';out.mkdir(exist_ok=True)
            result=subprocess.run(['btrfs','restore','-t',str(addr),'-r',str(rootid),str(disk),str(out)],capture_output=True,timeout=60)
            matched=[n for n in originals if (out/n).is_file() and (out/n).read_bytes()==originals[n] and sha(out/n)==hashes[n]]
            print(mode,gen,'matches',len(matched),'of',len(originals),flush=True)
            if len(matched)==len(originals):
                verified.append({'generation':gen,'tree_root':addr,'level':level})
        assert verified, ('No readable historical generation found',mode,prior,roots)
        packed=folder/'volume.raw.xz'
        with disk.open('rb') as r,lzma.open(packed,'wb',preset=6) as w:shutil.copyfileobj(r,w)
        assert packed.stat().st_size<2<<20
        report=dict(filesystem='Btrfs',subvolume=rootid,mode=mode,snapshots=0,current_generation=current,
                    before_unlink_generation=prior,verified_roots=verified,sha256=sha(disk),
                    compressed_sha256=sha(packed),bytes=disk.stat().st_size,originals=hashes)
        (folder/'manifest.json').write_text(json.dumps(report,indent=2)+'\n')


if __name__=='__main__':
    for mode in ('plain','zstd'):generate(mode)
