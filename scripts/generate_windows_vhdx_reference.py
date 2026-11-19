"""Create two disposable VHDX files in Windows, perform seven NTFS deletions, and hash the read-only guest disks."""
import hashlib
import json
import lzma
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile

from e2e import check
from e2e_restore_native import sync_volume

GOLD = Path(__file__).resolve().parents[1] / 'tests/corpus/files'
SIZE = 96 << 20


def generate(root, out, subformat):
    image = root / (subformat + '.vhdx')
    check(image.is_absolute() and not image.exists(), image)
    env = dict(os.environ, RESCUE_TEST_VHDX=str(image))

    def powershell(command):
        result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
                                 "$ErrorActionPreference='Stop';" + command],
                                env=env, check=True, capture_output=True, text=True, timeout=90)
        return result.stdout.strip()

    diskpart = root / (subformat + '.txt')
    create_command = f'create vdisk file="{image}" maximum=96 type={subformat}\n'
    style = 'mbr'
    if subformat == '4kn':
        from create_vhdx_4kn import create
        create(image, SIZE)
        create_command = ''
        style = 'gpt'
    diskpart.write_text(create_command +
                        f'select vdisk file="{image}"\nattach vdisk\nconvert {style}\n'
                        'create partition primary\nformat fs=ntfs quick label=RSC_VHDX\nassign\n',
                        encoding='ascii')
    subprocess.run(['diskpart.exe', '/s', diskpart], check=True, timeout=180)
    try:
        letter = powershell("""
            $image=Get-DiskImage -ImagePath $env:RESCUE_TEST_VHDX
            if(-not $image.Attached){throw 'test disk not attached'}
            $vol=@($image|Get-Disk|Get-Partition|Get-Volume|Where-Object FileSystemLabel -eq 'RSC_VHDX')
            if($vol.Count -ne 1){throw 'new test volume missing'}
            $vol[0].DriveLetter
        """)
        check(re.fullmatch('[A-Z]', letter), letter)
        mount = Path(letter + ':\\')
        folder = mount / 'rescue-vhdx-fixtures'
        folder.mkdir()
        originals = {}
        for ext in ('html', 'json', 'rtf', 'xml', 'png', 'zip', 'gz'):
            name = 'sample.' + ext
            data = (GOLD / name).read_bytes()
            (folder / name).write_bytes(data)
            check((folder / name).read_bytes() == data, name)
            originals[name] = hashlib.sha256(data).hexdigest()
        sync_volume(mount)
        for name in originals:
            (folder / name).unlink()
        sync_volume(mount)
        check(not list(folder.iterdir()), folder)
    finally:
        powershell('Dismount-DiskImage -ImagePath $env:RESCUE_TEST_VHDX')
    powershell('Mount-DiskImage -ImagePath $env:RESCUE_TEST_VHDX -Access ReadOnly -NoDriveLetter | Out-Null')
    try:
        owned = json.loads(powershell("""
            $d=Get-DiskImage -ImagePath $env:RESCUE_TEST_VHDX|Get-Disk
            if(-not $d.IsReadOnly){throw 'test image not read-only'}
            @{n=$d.Number; size=[int64]$d.Size; logical=$d.LogicalSectorSize; physical=$d.PhysicalSectorSize} | ConvertTo-Json -Compress
        """))
        check(owned['size'] == SIZE and isinstance(owned['n'], int) and owned['n'] > 0, owned)
        if subformat == '4kn':
            check(owned['logical'] == owned['physical'] == 4096, owned)
        digest = hashlib.sha256()
        blocks = []
        with open(r'\\.\PhysicalDrive' + str(owned['n']), 'rb', buffering=0) as device:
            for _ in range(SIZE >> 20):
                block = device.read(1 << 20)
                check(len(block) == 1 << 20, 'incomplete Windows logical read')
                digest.update(block)
                blocks.append(hashlib.sha256(block).hexdigest())
    finally:
        powershell('Dismount-DiskImage -ImagePath $env:RESCUE_TEST_VHDX')
    out.mkdir(parents=True)
    with image.open('rb') as source, lzma.open(out / 'volume.vhdx.xz', 'wb', preset=6) as compressed:
        while block := source.read(1 << 20):
            compressed.write(block)
    packed = out / 'volume.vhdx.xz'
    with image.open('rb') as src:
        source_hash = hashlib.file_digest(src, 'sha256').hexdigest()
    meta = dict(subformat=subformat, originals=originals, guest_bytes=SIZE, guest_sha256=digest.hexdigest(),
                logical_sector=owned['logical'], physical_sector=owned['physical'], guest_mib_sha256=blocks, container_bytes=image.stat().st_size, container_sha256=source_hash,
                packed_sha256=hashlib.sha256(packed.read_bytes()).hexdigest(), platform=platform.platform())
    (out / 'manifest.json').write_text(json.dumps(meta, indent=2) + '\n')
    print(subformat, 'seven NTFS deletions, read-only Windows guest SHA-256', digest.hexdigest(), flush=True)


if __name__ == '__main__':
    check(platform.system() == 'Windows', 'requires Windows virtual disk driver')
    out = Path('windows-vhdx-proof').resolve()
    with tempfile.TemporaryDirectory() as tmp:
        import sys
        for kind in (('4kn',) if '--4kn' in sys.argv else ('fixed', 'expandable')):
            generate(Path(tmp), out / kind, kind)
