"""Create a read-only btrfs snapshot and unlink corpus files on a disposable loop device."""
import hashlib
import json
import lzma
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def cmd(*args):
    result=subprocess.run(list(map(str,args)),capture_output=True,text=True,timeout=120)
    if result.returncode:raise RuntimeError(f'{args}: {result.stdout} {result.stderr}')
    return result.stdout


def sha(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()


def main():
    corpus=Path('tests/corpus/files')
    dest=Path('btrfs-fixture');dest.mkdir()
    names=['sample.'+e for e in ('png','xml','txt','zip','mp3','rtf','webp')]
    originals={name:sha(corpus/name) for name in names}
    with tempfile.TemporaryDirectory() as tmp:
        root=Path(tmp)
        image=root/'owned.raw'
        with image.open('wb') as f:f.truncate(160<<20)
        mount=root/'mnt';mount.mkdir()
        device=cmd('sudo','losetup','--find','--show',image).strip()
        assert re.fullmatch('/dev/loop[0-9]+',device)
        try:
            cmd('sudo','mkfs.btrfs','-q','-f','-L','RescueFixture','-m','single','-d','single',device)
            cmd('sudo','mount','-t','btrfs','-o','nodev,nosuid,compress=zlib',device,mount)
            try:
                live=mount/'Current';old=mount/'Before'
                cmd('sudo','btrfs','subvolume','create',live)
                cmd('sudo','chown',f'{os.getuid()}:{os.getgid()}',live)
                for name in names:shutil.copyfile(corpus/name,live/name)
                (live/'survivor.txt').write_bytes(b'survivor')
                cmd('sudo','btrfs','subvolume','snapshot','-r',live,old)
                live_id=int(cmd('sudo','btrfs','inspect-internal','rootid',live).strip())
                old_id=int(cmd('sudo','btrfs','inspect-internal','rootid',old).strip())
                for name in names:(live/name).unlink()
                (live/'created-later.txt').write_bytes(b'new file')
                assert all(not (live/name).exists() for name in names)
                assert all((old/name).read_bytes()==(corpus/name).read_bytes() and sha(old/name)==originals[name] for name in names)
                assert (old/'survivor.txt').read_bytes()==(live/'survivor.txt').read_bytes()
                assert not (old/'created-later.txt').exists()
                cmd('sudo','btrfs','filesystem','sync',mount)
            finally:cmd('sudo','umount',mount)
        finally:cmd('sudo','losetup','--detach',device)
        cmd('btrfs','check','--readonly',image)
        for sub,id in [('before',old_id),('current',live_id)]:
            out=root/sub;out.mkdir()
            cmd('btrfs','restore','-r',id,image,out)
            if sub=='before':
                for name,digest in originals.items():
                    assert (out/name).read_bytes()==(corpus/name).read_bytes() and sha(out/name)==digest
            else:assert all(not(out/name).exists() for name in names)
        packed=dest/'snapshot.raw.xz'
        with image.open('rb') as r,lzma.open(packed,'wb',preset=6) as w:shutil.copyfileobj(r,w)
        assert packed.stat().st_size<2<<20
        report=dict(filesystem='Btrfs',subvolume=live_id,snapshot=old_id,raw_bytes=image.stat().st_size,
                    sha256=sha(image),compressed_sha256=sha(packed),original_files=originals,
                    survivor='survivor.txt',created_later='created-later.txt')
        (dest/'manifest.json').write_text(json.dumps(report,indent=2)+'\n')
        print('Seven real unlinks checked with both the Linux Btrfs driver and offline btrfs restore',report,flush=True)


if __name__=='__main__':main()
