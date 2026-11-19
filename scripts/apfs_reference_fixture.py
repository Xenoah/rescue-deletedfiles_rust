"""Create a new disposable Apple APFS disk with surviving and actually removed files."""
import ctypes
import hashlib
import json
import lzma
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile


def command(*args, check=True):
    p = subprocess.run(list(map(str,args)), capture_output=True, check=False, timeout=90)
    if p.returncode and check:
        raise RuntimeError(f'{args[0:3]} returned {p.returncode}: '+(p.stdout+p.stderr).decode(errors='replace')[-3000:])
    return p


def create_snapshot(mount):
    # The name and fd both refer to this newly attached volume only.
    lib = ctypes.CDLL(None,use_errno=True)
    with directory_fd(mount) as fd:
        rc = lib.fs_snapshot_create(fd,b'RescueFixture',0)
        print('fs_snapshot_create:', rc, os.strerror(ctypes.get_errno()),flush=True)
    return rc == 0


from contextlib import contextmanager
@contextmanager
def directory_fd(path):
    fd = os.open(path, os.O_RDONLY)
    try: yield fd
    finally: os.close(fd)


def main(out):
    out.mkdir(parents=True,exist_ok=True)
    corpus = Path(__file__).resolve().parents[1]/'tests/corpus/files'
    with tempfile.TemporaryDirectory() as t:
        raw = Path(t)/'snapshot.raw'
        with raw.open('wb') as f: f.truncate(160*1024*1024)
        attach=plistlib.loads(command('hdiutil','attach','-nomount','-plist','-imagekey','diskimage-class=CRawDiskImage',raw).stdout)
        dev=attach['system-entities'][0]['dev-entry']
        if not re.fullmatch(r'/dev/disk\d+',dev):
            raise ValueError(dev)
        mount=None
        try:
            names = b'RescueFixture' + str(os.getpid()).encode()
            volume = names.decode()
            command('diskutil','eraseDisk','APFS',volume,'GPT',dev)
            mount=Path('/Volumes')/volume
            info=plistlib.loads(command('diskutil','info','-plist',mount).stdout)
            print('owned APFS volume', info.get('DeviceIdentifier'), info.get('FilesystemType'),flush=True)
            source=mount/'Rescue-Test';source.mkdir()
            command('touch',mount/'.metadata_never_index')
            files={}
            for ext in ('png','xml','txt','zip','mp3','rtf','webp'):
                data=(corpus/('sample.'+ext)).read_bytes()
                name='Rescue-Test/sample.'+ext
                (mount/name).write_bytes(data)
                files[name]=hashlib.sha256(data).hexdigest()
            (source/'survivor.txt').write_text('this file is live in both namespaces\n')
            command('sync')
            methods=[
                ['sudo',sys.executable,__file__,'--snapshot',str(mount)],
                ['sudo','diskutil','apfs','snapshot',str(mount),'-name','RescueFixture'],
                ['sudo','tmutil','localsnapshot',str(mount)]]
            for argv in methods:
                result=command(*argv,check=False)
                print('snapshot attempt',argv[1:3], result.returncode, (result.stdout+result.stderr).decode(errors='replace')[:900],flush=True)
                snap=plistlib.loads(command('diskutil','apfs','listSnapshots',mount,'-plist').stdout)
                print('snapshot records',snap,flush=True)
                if snap.get('Snapshots'): break
            else: raise RuntimeError('macOS did not create a snapshot on the owned temporary APFS volume')
            record=snap['Snapshots'][0]
            for name in files:(mount/name).unlink()
            (source/'created-later.txt').write_text('created after the snapshot\n')
            command('sync')
            assert all(not (mount/name).exists() for name in files)
            container=plistlib.loads(command('diskutil','apfs','list','-plist').stdout)['Containers']
            own=next(c for c in container if any(v.get('DeviceIdentifier')==info['DeviceIdentifier'] for v in c.get('Volumes',[])))
            store=own['PhysicalStores'][0]['DeviceIdentifier']
            partition=plistlib.loads(command('diskutil','info','-plist',store).stdout)
            print('partition data',store,partition.get('PartitionMapPartitionOffset'),partition.get('DeviceBlockSize'),flush=True)
            truth=dict(files=files,snapshot=record,sector_offset=partition['PartitionMapPartitionOffset']//512,
                       volume=volume,survivor='Rescue-Test/survivor.txt',created_later='Rescue-Test/created-later.txt')
        finally:
            command('hdiutil','detach',dev)
        data=raw.read_bytes()
        packed=lzma.compress(data,preset=4)
        assert len(packed)<16*1024*1024
        (out/'snapshot.raw.xz').write_bytes(packed)
        truth.update(raw_bytes=len(data),sha256=hashlib.sha256(data).hexdigest(),compressed_sha256=hashlib.sha256(packed).hexdigest())
        (out/'manifest.json').write_text(json.dumps(truth,indent=2,default=str)+'\n')
        print('SEVEN ACTUAL DELETIONS',len(packed),'compressed image bytes',truth['sha256'],flush=True)

if __name__=='__main__':
    if sys.argv[1]=='--snapshot':
        sys.exit(0 if create_snapshot(sys.argv[2]) else 1)
    main(Path(sys.argv[1]).resolve())
