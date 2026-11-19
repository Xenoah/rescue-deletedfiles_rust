"""Acquire a committed post-unlink ext4 volume with real kernel JBD2 history."""
import hashlib
import json
import lzma
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from ext4_sparse_samples import samples, write_sparse


def command(*args):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise RuntimeError(f'{args}: {result.stdout} {result.stderr}')
    return result.stdout


def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def generate(block_size):
    originals = samples()
    expected = {n: hashlib.sha256(b).hexdigest() for n,b in originals.items()}
    folder = Path('journal-extents-fixture')/str(block_size)
    folder.mkdir(parents=True)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        disk = work/'owned.raw'
        with disk.open('wb') as f:
            f.truncate(96 << 20)
        mount = work/'mnt'
        mount.mkdir()
        device = command('sudo', 'losetup', '--find', '--show', disk).strip()
        assert re.fullmatch('/dev/loop[0-9]+', device)
        try:
            command('sudo', 'mkfs.ext4', '-q', '-F', '-b', str(block_size), '-I', '256', '-J', 'size=8',
                    '-E', 'lazy_itable_init=0,lazy_journal_init=0', device)
            command('sudo', 'mount', '-t', 'ext4', '-o', 'nodev,nosuid,nodiscard,data=ordered,journal_checksum', device, mount)
            try:
                deleted = mount/'Measurements'
                command('sudo', 'mkdir', deleted)
                command('sudo', 'chown', f'{os.getuid()}:{os.getgid()}', deleted)
                for name, data in originals.items():
                    write_sparse(deleted/name,data,block_size)
                (deleted/'survivor').write_bytes(b'survivor remains allocated\n' * 4000)
                command('sync', '-f', mount)
                assert all((deleted/name).read_bytes() == data and sha(deleted/name) == expected[name] for name,data in originals.items())
                ino = {name: (deleted/name).stat().st_ino for name in originals}
                extent_counts={}
                for name in originals:
                    listing=command('sudo','filefrag','-v','-b'+str(block_size),deleted/name)
                    count=len(re.findall(r'^\s*\d+:',listing,re.M))
                    assert count>4,(name,listing)
                    extent_counts[name]=count
                parent = deleted.stat().st_ino
                for name in originals:
                    (deleted/name).unlink()
                command('sync', '-f', mount)
                assert set(p.name for p in deleted.iterdir()) == {'survivor'}
                copied = work/'post-unlink.raw'
                command('sudo','fsfreeze','--freeze',mount)
                try:
                    command('sudo','dd','if='+device,'of='+str(copied),'bs=4M','status=none')
                finally:
                    command('sudo','fsfreeze','--unfreeze',mount)
                command('sudo','chown',f'{os.getuid()}:{os.getgid()}',copied)
            finally:
                command('sudo','umount',mount)
        finally:
            command('sudo','losetup','--detach',device)
        listing=command('debugfs','-R','ls -p /Measurements',copied)
        assert '/survivor/' in listing and all('/'+name+'/' not in listing for name in originals)
        journal=work/'journal.bin'
        command('debugfs','-R','dump <8> '+str(journal),copied)
        raw=journal.read_bytes()
        assert raw.startswith(bytes.fromhex('c03b3998')) and len(raw)==8<<20
        header=command('dumpe2fs','-h',copied)
        packed=folder/'volume.raw.xz'
        with copied.open('rb') as src,lzma.open(packed,'wb') as dst:
            shutil.copyfileobj(src,dst)
        meta=dict(filesystem='ext4',allocated_extents=extent_counts,block_size=block_size,originals=expected,unlinked_inodes=ino,parent_inode=parent,
                  sha256=sha(copied),compressed_sha256=sha(packed),bytes=copied.stat().st_size,journal_sha256=sha(journal),
                  filesystem_features=re.search(r'^Filesystem features:\s*(.*)$',header,re.M)[1],
                  journal_features=re.search(r'^Journal features:\s*(.*)$',header,re.M)[1])
        (folder/'manifest.json').write_text(json.dumps(meta,indent=2)+'\n')
        print(block_size,meta['journal_features'],len(ino),'real unlinks, journal',sha(journal),flush=True)


if __name__ == '__main__':
    for size in (1024,4096):
        generate(size)
