"""Encode corpus files using Windows MakeCAB, then independently verify each member using Windows Expand."""
import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


def sha(data):
    return hashlib.sha256(data).hexdigest()


def create(output):
    corpus = Path(__file__).resolve().parents[1] / 'tests/corpus/files'
    output.mkdir(parents=True, exist_ok=True)
    payload = {'doc.rtf': (corpus / 'sample.rtf').read_bytes(), 'logo.png': (corpus / 'sample.png').read_bytes(),
               'data.xml': (corpus / 'sample.xml').read_bytes(), 'table.csv': (corpus / 'sample.csv').read_bytes(),
               'large.bin': bytes(range(251)) * 547, 'void.txt': b''}
    modes = [('mszip', 'MSZIP', 0, False), ('lzx15', 'LZX', 15, False),
             ('lzx18', 'LZX', 18, False), ('lzx21-folders', 'LZX', 21, True)]
    manifest = {}
    for name, codec, bits, split in modes:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            entries = list(payload)
            settings = ['.OPTION EXPLICIT', '.Set Cabinet=ON', '.Set Compress=ON',
                        '.Set CabinetNameTemplate=windows.cab', '.Set DiskDirectoryTemplate=out',
                        '.Set MaxDiskSize=0', '.Set MaxCabinetSize=0', '.Set CompressionType=' + codec]
            if bits:
                settings.append(f'.Set CompressionMemory={bits}')
            for i, entry in enumerate(entries):
                (root / entry).write_bytes(payload[entry])
                if split and i == 3:
                    settings.append('.New Folder')
                settings.append(entry)
            (root/'disk.ddf').write_text('\n'.join(settings)+'\n', encoding='ascii')
            subprocess.run(['makecab.exe', '/F', 'disk.ddf'], cwd=root, check=True)
            cabinet = root / 'out/windows.cab'
            restored = root / 'expanded'
            restored.mkdir()
            subprocess.run(['expand.exe', cabinet, '-F:*', restored], check=True)
            assert sorted(p.name for p in restored.iterdir()) == sorted(entries)
            for entry in entries:
                assert (restored/entry).read_bytes() == payload[entry]
            target = output / ('win-'+name+'.cab')
            shutil.copyfile(cabinet, target)
            manifest[target.name] = dict(sha256=sha(target.read_bytes()), bytes=target.stat().st_size,
                                         producer='Windows MakeCAB', codec=codec, window=bits, split_folder=split,
                                         members={entry:sha(data) for entry,data in payload.items()})
    (output/'windows.json').write_text(json.dumps(manifest, indent=2)+'\n', encoding='utf-8')


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    create(p.parse_args().output.resolve())
