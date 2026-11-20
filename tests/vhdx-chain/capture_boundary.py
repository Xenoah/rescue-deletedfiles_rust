"""Create two disposable NTFS partitions straddling the 4 GiB VHDX bitmap boundary."""
from contextlib import contextmanager
import hashlib
import json
import lzma
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent/'vhd-chain'))
from generate_windows import archive, check, create, ps, sync_volume, NAMES

MIB = 1 << 20
SIZE = 4192 * MIB


@contextmanager
def attach(image, readonly=False):
    ps(image, 'Mount-DiskImage -ImagePath $env:RESCUE_OWNED_VHD ' +
       ('-Access ReadOnly -NoDriveLetter' if readonly else '-Access ReadWrite') + ' | Out-Null')
    try:
        if readonly:
            disk = json.loads(ps(image, """
            $d=Get-DiskImage -ImagePath $env:RESCUE_OWNED_VHD | Get-Disk
            if(-not $d.IsReadOnly){throw 'fixture was opened writable'}
            @{number=$d.Number;size=[long]$d.Size;logical=$d.LogicalSectorSize}|ConvertTo-Json -Compress
            """))
            check(disk['number'] > 0 and disk['size'] == SIZE and disk['logical'] == 512, disk)
            yield disk
        else:
            parts = json.loads(ps(image, """
            $parts=@(Get-DiskImage -ImagePath $env:RESCUE_OWNED_VHD | Get-Disk | Get-Partition)
            $entries=@(foreach($p in $parts) {
              $v=$p | Get-Volume -ErrorAction SilentlyContinue
              if($v -and $v.FileSystemLabel -in @('RSC_VX_LOW','RSC_VX_HIGH')){
                if(-not $v.DriveLetter){$p | Add-PartitionAccessPath -AssignDriveLetter; $v=$p|Get-Volume}
                @{label=$v.FileSystemLabel;letter=[string]$v.DriveLetter;offset=[long]$p.Offset}
              }
            })
            ConvertTo-Json -InputObject $entries -Compress
            """))
            parts = sorted(parts, key=lambda p:p['offset'])
            check(len(parts)==2 and [p['label'] for p in parts]==['RSC_VX_LOW','RSC_VX_HIGH'], parts)
            check([p['offset'] for p in parts]==[4036*MIB,4098*MIB] and all(p['letter'].isalpha() and len(p['letter']) == 1 for p in parts), parts)
            yield [Path(p['letter']+':\\') for p in parts]
    finally:
        ps(image,'Dismount-DiskImage -ImagePath $env:RESCUE_OWNED_VHD')


def logical(image, folder):
    """Keep full guest bytes in a sparse, per-MiB encoding with an implicit all-zero default."""
    h = hashlib.sha256(); blocks = []; nz = []; zero = bytes(MIB)
    with attach(image, True) as disk, open(r'\\.\PhysicalDrive'+str(disk['number']), 'rb', buffering=0) as source:
        outpath=folder/(image.name+'.nonzero.xz')
        with lzma.open(outpath,'xb',preset=3) as out:
            for i in range(SIZE//MIB):
                data=source.read(MIB);check(len(data)==MIB, i)
                h.update(data);blocks.append(hashlib.sha256(data).hexdigest())
                if data != zero:
                    out.write(data);nz.append(i)
    return dict(guest_bytes=SIZE,guest_sha256=h.hexdigest(),guest_mib_sha256=blocks,
                nonzero_mib=nz,nonzero_xz_sha256=hashlib.sha256(outpath.read_bytes()).hexdigest())


def main():
    check(platform.system()=='Windows','requires Windows')
    out=ROOT/'boundary-4g';out.mkdir()
    originals = {n:(ROOT.parent/'corpus/files'/n).read_bytes() for n in NAMES}
    with tempfile.TemporaryDirectory() as temp:
        root=Path(temp);base=root/'base.vhdx';child=root/'child.vhdx'
        script=root/'create.txt';commands=[f'create vdisk file="{base}" maximum=4192 type=expandable',f'select vdisk file="{base}"','attach vdisk','convert gpt']
        for label,start in [('RSC_VX_LOW',4036),('RSC_VX_HIGH',4098)]:
            commands += [f'create partition primary size=54 offset={start*1024}',
                         'format fs=ntfs quick label='+label,'assign']
        script.write_text('\n'.join(commands+['detach vdisk'])+'\n',encoding='ascii')
        subprocess.run(['diskpart','/s',script],check=True,timeout=180)
        with attach(base) as volumes:
            for idx, mount in enumerate(volumes):
                dest=mount/'deletions';dest.mkdir()
                for name, data in originals.items(): (dest/name).write_bytes(data)
                (mount/'sentinel.bin').write_bytes(bytes([80+idx])*4096)
                sync_volume(mount)
                check(all((dest/name).read_bytes()==data for name,data in originals.items()), mount)
        baseproof=logical(base,out)
        create(child,'differencing',base)
        with attach(child) as volumes:
            for i,mount in enumerate(volumes):
                with (mount/'sentinel.bin').open('r+b') as f:
                    f.write(bytes([i+1])*512);f.flush();os.fsync(f.fileno())
                for n,data in originals.items():
                    f=mount/'deletions'/n;check(f.read_bytes()==data,n);f.unlink()
                check(not list((mount/'deletions').iterdir()), mount);sync_volume(mount)
        childproof=logical(child,out)
        specs=[dict(**archive(image,out),**proof) for image,proof in [(base,baseproof),(child,childproof)]]
        check(baseproof['guest_sha256']!=childproof['guest_sha256'],specs)
        # Independently identify all changed guest MiBs, including at least one on each side.
        changed=[i for i,(a,b) in enumerate(zip(baseproof['guest_mib_sha256'],childproof['guest_mib_sha256'])) if a!=b]
        check(any(i<4096 for i in changed) and any(i>=4096 for i in changed), changed)
        (out/'manifest.json').write_text(json.dumps(dict(windows=platform.platform(), members=specs,
            changed_guest_mib=changed,partition_mib=[4036,4098],
            originals={n:hashlib.sha256(b).hexdigest() for n,b in originals.items()}),indent=2)+'\n')
        print('Fourteen native NTFS unlinks in two physical partitions, guest changes on both sides of 4 GiB:',changed)


if __name__=='__main__':
    main()
