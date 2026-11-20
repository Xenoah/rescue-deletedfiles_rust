"""Capture Windows-created differencing VHDs and the logical bytes presented by the Windows driver."""
from contextlib import contextmanager
import hashlib
import json
import lzma
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parents[1] / 'scripts'))
from e2e import check
from e2e_restore_native import sync_volume

NAMES = ['sample.' + ext for ext in ('html', 'json', 'rtf', 'xml', 'png', 'zip', 'gz')]
LABEL = 'RSC_CHAIN'


def ps(image, cmd):
    return subprocess.check_output(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
                                    "$ErrorActionPreference='Stop';" + cmd],
                                   env=dict(os.environ, RESCUE_OWNED_VHD=str(image)), text=True, timeout=120).strip()


def create(image, kind, parent=None):
    check(image.is_absolute() and not image.exists(), image)
    command = [f'create vdisk file="{image}" ' + (f'parent="{parent}"' if parent else 'maximum=96 type=' + kind)]
    if not parent:
        command += [f'select vdisk file="{image}"', 'attach vdisk', 'convert mbr', 'create partition primary',
                    f'format fs=ntfs quick label={LABEL}', 'assign', 'detach vdisk']
    script = image.with_suffix('.txt')
    script.write_text('\n'.join(command)+'\n', encoding='ascii')
    subprocess.run(['diskpart.exe', '/s', script], timeout=180, check=True)
    check(image.is_file(), image)


@contextmanager
def attached(image, readonly=False):
    args = '-Access ReadOnly -NoDriveLetter' if readonly else '-Access ReadWrite'
    ps(image, 'Mount-DiskImage -ImagePath $env:RESCUE_OWNED_VHD ' + args + ' | Out-Null')
    try:
        if readonly:
            owned = json.loads(ps(image, """
              $d=Get-DiskImage -ImagePath $env:RESCUE_OWNED_VHD|Get-Disk
              if(-not $d.IsReadOnly){throw 'image is writable'}
              @{number=$d.Number; size=[int64]$d.Size}|ConvertTo-Json -Compress
            """))
            check(type(owned['number']) is int and owned['number']>0 and (90<<20)<=owned['size']<=(96<<20), owned)
            yield owned
        else:
            letter = ps(image, """
              $image=Get-DiskImage -ImagePath $env:RESCUE_OWNED_VHD
              if(-not $image.Attached){throw 'owned image not attached'}
              $vol=@($image|Get-Disk|Get-Partition|Get-Volume|Where-Object FileSystemLabel -eq 'RSC_CHAIN')
              if($vol.Count -ne 1){throw 'owned NTFS volume missing'}
              if(-not $vol[0].DriveLetter) {
                $p=$vol[0]|Get-Partition
                $p|Add-PartitionAccessPath -AssignDriveLetter
                $vol=@($p|Get-Volume)
              }
              $vol[0].DriveLetter
            """)
            check(re.fullmatch('[A-Z]', letter), letter)
            yield Path(letter + ':\\')
    finally:
        ps(image, 'Dismount-DiskImage -ImagePath $env:RESCUE_OWNED_VHD')


def logical(image):
    digest = hashlib.sha256()
    hashes = []
    with attached(image, readonly=True) as owned:
        with open(r'\\.\PhysicalDrive' + str(owned['number']), 'rb', buffering=0) as source:
            for offset in range(0, owned['size'], 1<<20):
                length = min(1<<20, owned['size'] - offset)
                chunk = source.read(length)
                check(len(chunk) == length, (image, offset))
                digest.update(chunk)
                hashes.append(hashlib.sha256(chunk).hexdigest())
    return dict(guest_bytes=owned['size'], guest_sha256=digest.hexdigest(), guest_mib_sha256=hashes)


def archive(image, out):
    dest = out/(image.name+'.xz')
    with image.open('rb') as src, lzma.open(dest, 'wb', preset=5) as packed:
        shutil.copyfileobj(src, packed, length=1<<20)
    def sha(path):
        with path.open('rb') as f:
            return hashlib.file_digest(f, 'sha256').hexdigest()
    return dict(filename=image.name, bytes=image.stat().st_size, sha256=sha(image), xz_sha256=sha(dest))


def capture(root, case, kind, depth, layered=False):
    folder = root/case
    folder.mkdir()
    base = folder/'base.vhd'
    create(base, kind)
    names = ['sample.'+ext for ext in ('avi','docx','flac','pptx','sqlite','tar','wav')] if layered else NAMES
    originals = {name:(ROOT.parent/'corpus/files'/name).read_bytes() for name in names}
    def contents(name, level):
        body = bytearray(originals[name])
        if layered:
            for rev, start in [(1,512), (2,4096)]:
                check(len(body) >= start+512, name)
                if level < rev:
                    body[start:start+512] = bytes(b ^ 0x39 for b in body[start:start+512])
        return bytes(body)
    marker = (bytes(range(1, 251))*9000)[:2<<20]
    with attached(base) as mount:
        target=mount/'deletions'
        target.mkdir()
        for name, data in originals.items():
            (target/name).write_bytes(contents(name, 0))
        (mount/'write-control.bin').write_bytes(marker)
        sync_volume(mount)
        check(all((target/n).read_bytes() == contents(n,0) for n in originals), case)
    disks = [base]
    proofs = [logical(base)]
    for level in range(1, depth+1):
        image = folder/f'child{level}.vhd'
        create(image, 'differencing', disks[-1])
        with attached(image) as mount:
            check(all((mount/'deletions'/name).read_bytes() == contents(name,level-1) for name in originals), case)
            if layered:
                offset = 512 if level == 1 else 4096
                for name, raw in originals.items():
                    with (mount/'deletions'/name).open('r+b') as f:
                        f.seek(offset)
                        f.write(raw[offset:offset+512])
                    check((mount/'deletions'/name).read_bytes() == contents(name, level), name)
            with (mount/'write-control.bin').open('r+b') as control:
                at = level * 4096
                control.seek(at)
                check(control.read(512) == marker[at:at+512], case)
                control.seek(at)
                control.write(bytes(512))
                control.seek(at+1024)
                control.write(bytes([level])*512)
            if level == depth:
                for name in originals:
                    (mount/'deletions'/name).unlink()
                check(not list((mount/'deletions').iterdir()), case)
            sync_volume(mount)
        disks.append(image)
        proofs.append(logical(image))
    out=ROOT/case
    out.mkdir()
    result = [dict(**archive(image,out), **proof) for image,proof in zip(disks,proofs)]
    check(len({v['guest_sha256'] for v in result}) == len(disks), case)
    reference = dict(windows=platform.platform(), parent_type=kind, members=result,
                     originals={n:hashlib.sha256(v).hexdigest() for n,v in originals.items()})
    (out/'manifest.json').write_text(json.dumps(reference, indent=2)+'\n', encoding='utf-8')
    print(case, 'seven real NTFS unlinks, Windows logical guest SHA-256', proofs[-1]['guest_sha256'], flush=True)


if __name__=='__main__':
    check(platform.system()=='Windows', 'requires Windows VHD driver')
    with tempfile.TemporaryDirectory() as tmp:
        for case,kind,depth in [('fixed-child','fixed',1),('dynamic-child','expandable',1),
                                 ('dynamic-two-children','expandable',2)]:
            capture(Path(tmp),case,kind,depth)
