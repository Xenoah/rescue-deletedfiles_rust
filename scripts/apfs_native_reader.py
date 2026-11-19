"""Mount the generated APFS current volume and retained snapshot read-only on macOS."""
import hashlib
import json
import lzma
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile
import time

def run(*args):
    p=subprocess.run(list(map(str,args)),capture_output=True,timeout=45)
    if p.returncode:raise RuntimeError(f'{args}: '+(p.stdout+p.stderr).decode(errors='replace')[-2000:])
    return p.stdout

def main():
    fixture=Path('tests/apfs-reference')
    meta=json.loads((fixture/'manifest.json').read_text())
    with tempfile.TemporaryDirectory() as tmp:
        root=Path(tmp);disk=root/'owned.raw'
        with lzma.open(fixture/'snapshot.raw.xz','rb') as src,disk.open('wb') as dst:shutil.copyfileobj(src,dst)
        with disk.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==meta['sha256']
        attach=plistlib.loads(run('hdiutil','attach','-nomount','-readonly','-plist','-imagekey','diskimage-class=CRawDiskImage',disk))
        dev=attach['system-entities'][0]['dev-entry']
        assert re.fullmatch(r'/dev/disk\d+',dev)
        try:
            volume=None
            for _ in range(15):
                containers=plistlib.loads(run('diskutil','apfs','list','-plist'))['Containers']
                for c in containers:
                    stores=[v.get('DeviceIdentifier','') for v in c.get('PhysicalStores',[])]
                    if dev.removeprefix('/dev/') not in stores:continue
                    volume=next((v['DeviceIdentifier'] for v in c['Volumes'] if v.get('Name')==meta['volume']),None)
                if volume:break
                time.sleep(.5)
            assert volume, (dev,attach)
            reported=plistlib.loads(run('diskutil','apfs','listSnapshots',volume,'-plist'))
            print('Actual macOS snapshots',reported,flush=True)
            assert any(s.get('SnapshotName')==meta['snapshot'] or s.get('Name')==meta['snapshot'] for s in reported['Snapshots'])
            for which in ('current','snapshot'):
                mount=root/which;mount.mkdir()
                flags=['-s',meta['snapshot']] if which=='snapshot' else []
                run('sudo','/sbin/mount_apfs','-o','rdonly,nobrowse',*flags,'/dev/'+volume,mount)
                try:
                    assert (mount/meta['survivor']).is_file()
                    if which=='current':
                        assert (mount/meta['created_later']).is_file() and all(not (mount/name).exists() for name in meta['original_files'])
                    else:
                        assert not (mount/meta['created_later']).exists()
                        assert all(hashlib.sha256((mount/name).read_bytes()).hexdigest()==digest for name,digest in meta['original_files'].items())
                finally:run('sudo','/sbin/umount',mount)
            print('macOS mounted both namespaces read-only and verified all seven original SHA-256 values',flush=True)
        finally:run('hdiutil','detach',dev)

if __name__=='__main__':main()
