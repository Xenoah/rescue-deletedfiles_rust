"""Capture real Linux ext4 unlinks and still-live byte-identical copies on owned file-backed mounts."""
from contextlib import contextmanager
import hashlib
import json
import lzma
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
FILES = {name: (ROOT.parent/'corpus/files'/name).read_bytes()
         for name in ('sample.bmp','sample.gif','sample.png','sample.pdf','sample.webp','sample.wav','sample.zip')}


def command(*args):
    return subprocess.run(list(map(str,args)), check=True, timeout=180)


@contextmanager
def mounted(image, directory):
    command('mount', '-t', 'ext4', '-o', 'loop,nodiscard', image, directory)
    try:
        yield
    finally:
        command('umount', directory)


def locations(data, file):
    hits = []
    start = 0
    while (pos := data.find(file, start)) >= 0:
        hits.append(pos)
        start = pos + 1
    return hits


def generate(folder, block, journal):
    image, mount = folder/'volume.raw', folder/'volume'
    mount.mkdir()
    with image.open('xb') as out:
        out.truncate(32 << 20)
    features = ['-O','64bit,metadata_csum,extent,'+('has_journal' if journal else '^has_journal')]
    command('mke2fs','-q','-F','-t','ext4','-b',block,*features,'-E','lazy_itable_init=0,lazy_journal_init=0',image)
    with mounted(image,mount):
        (mount/'deleted').mkdir()
        (mount/'live').mkdir()
        for name, data in FILES.items():
            file = mount/'deleted'/name
            file.write_bytes(data)
            assert file.read_bytes() == data
    first = image.read_bytes()
    expected = {name:locations(first,data) for name,data in FILES.items()}
    assert all(len(hits)==1 for hits in expected.values()),expected
    with mounted(image,mount):
        for name,data in FILES.items():
            control = mount/'live'/name
            control.write_bytes(data)
            assert control.read_bytes() == (mount/'deleted'/name).read_bytes() == data
        os.sync()
        for name in FILES:
            (mount/'deleted'/name).unlink()
        assert not list((mount/'deleted').iterdir())
        assert {p.name for p in (mount/'live').iterdir()} == FILES.keys()
    data = image.read_bytes()
    command('e2fsck','-f','-n',image)
    assert image.read_bytes() == data
    originals = {}
    for name,file in FILES.items():
        old, = expected[name]
        hits = locations(data,file)
        new, = [pos for pos in hits if pos!=old]
        originals[name] = dict(bytes=len(file),sha256=hashlib.sha256(file).hexdigest(),
                               deleted_offset=old,live_offset=new,deleted_bytes_retained=old in hits)
    stem = f'ext4-{block}'
    (ROOT/(stem+'.raw.xz')).write_bytes(lzma.compress(data))
    print(stem, 'retained', sum(v['deleted_bytes_retained'] for v in originals.values()), 'of',len(FILES),flush=True)
    return dict(bytes=len(data),block_size=block,has_journal=journal,disk_sha256=hashlib.sha256(data).hexdigest(),
                originals=originals)


if __name__=='__main__':
    manifest={}
    for block,journal in [(1024,True),(2048,False),(4096,True)]:
        with tempfile.TemporaryDirectory() as tmp:
            manifest[f'ext4-{block}']=generate(Path(tmp),block,journal)
    (ROOT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
