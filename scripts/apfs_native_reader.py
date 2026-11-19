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

def attached_volume(entities, containers, name):
    # hdiutil may report the synthesized container and volume before their physical store.
    attached = {item['dev-entry'].removeprefix('/dev/') for item in entities
                if re.fullmatch(r'/dev/disk\d+(s\d+)?', item.get('dev-entry', ''))}
    matches = [(store['DeviceIdentifier'], volume['DeviceIdentifier'])
               for container in containers
               for store in container.get('PhysicalStores', [])
               if store.get('DeviceIdentifier') in attached
               for volume in container.get('Volumes', [])
               if volume.get('Name') == name and re.fullmatch(r'disk\d+s\d+', volume['DeviceIdentifier'])]
    if len(matches) > 1:
        raise ValueError(f'ambiguous attached APFS volume: {matches}')
    return matches[0] if matches else None


def main():
    fixture=Path('tests/apfs-reference')
    meta=json.loads((fixture/'manifest.json').read_text())
    with tempfile.TemporaryDirectory() as tmp:
        root=Path(tmp);disk=root/'owned.raw'
        with lzma.open(fixture/'snapshot.raw.xz','rb') as src,disk.open('wb') as dst:shutil.copyfileobj(src,dst)
        with disk.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==meta['sha256']
        attach=plistlib.loads(run('hdiutil','attach','-nomount','-readonly','-plist','-imagekey','diskimage-class=CRawDiskImage',disk))
        entities = attach['system-entities']
        whole = [item['dev-entry'] for item in entities if re.fullmatch(r'/dev/disk\d+', item.get('dev-entry', ''))]
        assert whole, attach
        dev = whole[0]
        try:
            for _ in range(15):
                containers = plistlib.loads(run('diskutil', 'apfs', 'list', '-plist'))['Containers']
                match = attached_volume(entities, containers, meta['volume'])
                if match:
                    store, volume = match
                    dev = '/dev/' + store
                    break
                time.sleep(.5)
            else:
                raise AssertionError((attach, containers))
            for which in ('current','snapshot'):
                mount=root/which;mount.mkdir()
                flags=['-s',meta['snapshot']] if which=='snapshot' else []
                run('sudo','/sbin/mount_apfs','-o','rdonly,nobrowse',*flags,'/dev/'+volume,mount)
                try:
                    assert (mount/meta['survivor']).is_file()
                    if which=='current':
                        reported=plistlib.loads(run('diskutil','apfs','listSnapshots',mount,'-plist'))
                        print('Actual macOS snapshots',reported,flush=True)
                        assert any(s.get('SnapshotName')==meta['snapshot'] or s.get('Name')==meta['snapshot'] for s in reported['Snapshots'])
                        assert (mount/meta['created_later']).is_file() and all(not (mount/name).exists() for name in meta['original_files'])
                    else:
                        assert not (mount/meta['created_later']).exists()
                        for name,digest in meta['original_files'].items():
                            original=(fixture.parent/'corpus/files'/Path(name).name).read_bytes()
                            restored=(mount/name).read_bytes()
                            assert restored==original and hashlib.sha256(restored).hexdigest()==digest
                finally:run('sudo','/sbin/umount',mount)
            print('macOS mounted both namespaces read-only and verified all seven original SHA-256 values',flush=True)
        finally:run('hdiutil','detach',dev)

if __name__=='__main__':main()
