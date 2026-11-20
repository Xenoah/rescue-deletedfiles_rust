"""Create genuine unmounted HFS+ and case-sensitive HFSX volumes on owned macOS disk images."""
from contextlib import contextmanager
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
CORPUS = ROOT.parent/'corpus/files'
FILES = {f'sample.{ext}':(CORPUS/f'sample.{ext}').read_bytes() for ext in
         ('bmp','gif','png','pdf','webp','wav','zip')}


def command(*args):
    result = subprocess.run(list(map(str,args)),capture_output=True,timeout=120)
    if result.returncode:
        raise RuntimeError(f'{args[0]} exited {result.returncode}: {result.stderr.decode(errors="replace")}')
    return result.stdout


@contextmanager
def owned_disk(path):
    image = plistlib.loads(command('hdiutil','attach','-plist','-nomount','-imagekey',
                                  'diskimage-class=CRawDiskImage',path))
    dev = next(e['dev-entry'] for e in image['system-entities'] if re.fullmatch(r'/dev/disk\d+',e.get('dev-entry','')))
    # Only detach the device created by this exact hdiutil attach call.
    try:
        yield dev
    finally:
        command('hdiutil','detach',dev)


@contextmanager
def mounted(dev, path):
    command('/sbin/mount_hfs','-o','nobrowse,noowners',dev,path)
    try:
        yield
    finally:
        os.sync()
        command('/sbin/umount',path)


def offsets(disk, needle):
    hits=[]
    pos=disk.find(needle)
    while pos!=-1:
        hits.append(pos)
        pos=disk.find(needle,pos+1)
    return hits


def generate(case, work):
    image, mount = work/'volume.raw',work/'mount'
    mount.mkdir()
    with image.open('wb') as f:
        f.truncate(32<<20)
    with owned_disk(image) as dev:
        command('/sbin/newfs_hfs',*(['-s'] if case.startswith('hfsx') else []),
                *(['-J','1M'] if case.endswith('journaled') else []),'-v','Rescue QA',dev.replace('/dev/disk','/dev/rdisk'))
        with mounted(dev,mount):
            (mount/'.metadata_never_index').touch()
            (mount/'deleted').mkdir()
            for name,data in FILES.items():
                p=mount/'deleted'/name
                p.write_bytes(data)
                assert p.read_bytes()==data
    first = image.read_bytes()
    original={name:offsets(first,data) for name,data in FILES.items()}
    assert all(len(locations)==1 for locations in original.values()),original
    with owned_disk(image) as dev:
        with mounted(dev,mount):
            (mount/'live').mkdir()
            for name,data in FILES.items():
                p=mount/'live'/name
                p.write_bytes(data)
                assert p.read_bytes()==(mount/'deleted'/name).read_bytes()==data
            for name in FILES:
                (mount/'deleted'/name).unlink()
            assert not list((mount/'deleted').iterdir())
            assert sorted(p.name for p in (mount/'live').iterdir())==sorted(FILES)
    disk=image.read_bytes()
    manifest={}
    for name,data in FILES.items():
        before=original[name][0]
        retained=offsets(disk,data)
        live=[at for at in retained if at!=before]
        assert len(live)==1,(name,before,retained)
        manifest[name]=dict(sha256=hashlib.sha256(data).hexdigest(),bytes=len(data),deleted_offset=before,
                            deleted_bytes_retained=before in retained,live_offset=live[0])
    (ROOT/(case+'.raw.xz')).write_bytes(lzma.compress(disk))
    report=dict(bytes=len(disk),disk_sha256=hashlib.sha256(disk).hexdigest(),
                signature=disk[1024:1026].decode('ascii'),originals=manifest)
    print(case,report['signature'],sum(v['deleted_bytes_retained'] for v in manifest.values()),
          'of',len(FILES),'unlinked file bodies remained',flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--journaled-only',action='store_true')
    options=parser.parse_args()
    reports={}
    cases=('hfsplus-journaled','hfsx-journaled') if options.journaled_only else ('hfsplus','hfsx')
    for case in cases:
        with tempfile.TemporaryDirectory() as tmp:
            reports[case]=generate(case,Path(tmp))
    (ROOT/('journaled.json' if options.journaled_only else 'manifest.json')).write_text(json.dumps(reports,indent=2)+'\n')
