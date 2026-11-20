"""Reopen relocated VHD chains read-only on Windows and preserve a full logical-byte oracle."""
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import tempfile

from generate_windows import ROOT, attached, check


def read_guest(image, spec, output):
    digest = hashlib.sha256()
    blocks = []
    with attached(image, readonly=True) as owned:
        check(owned['size'] == spec['guest_bytes'], owned)
        with open(r'\\.\PhysicalDrive' + str(owned['number']), 'rb', buffering=0) as device, lzma.open(output,'xb',preset=5) as dst:
            for position in range(0, owned['size'], 1<<20):
                length = min(1<<20, owned['size']-position)
                chunk = device.read(length)
                check(len(chunk) == length, position)
                digest.update(chunk)
                blocks.append(hashlib.sha256(chunk).hexdigest())
                dst.write(chunk)
    check(digest.hexdigest() == spec['guest_sha256'] and blocks == spec['guest_mib_sha256'], image)
    with output.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


if __name__=='__main__':
    for ref in sorted(ROOT.glob('*/manifest.json')):
        data=json.loads(ref.read_text())
        result={}
        with tempfile.TemporaryDirectory() as tmp:
            for spec in data['members']:
                name=spec['filename']
                image=Path(tmp)/name
                with lzma.open(ref.parent/(name+'.xz')) as z, image.open('xb') as src:
                    shutil.copyfileobj(z, src, length=1<<20)
                with image.open('rb') as f:
                    check(hashlib.file_digest(f, 'sha256').hexdigest() == spec['sha256'], image)
                out=ref.parent/(name+'.guest.raw.xz')
                result[out.name]=read_guest(image,spec,out)
                print(ref.parent.name, name, spec['guest_sha256'], 'relocated driver readback matches',flush=True)
        (ref.parent/'driver.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
