"""Capture real NTFS unlinks of seven multi-cluster documents and media in a two-child VHD."""
import json
import lzma
from pathlib import Path
import shutil
import tempfile
from capture_driver_raw import read_guest
from generate_windows import ROOT, capture

with tempfile.TemporaryDirectory() as tmp:
    capture(Path(tmp), 'dynamic-nonresident', 'expandable', 2, layered=True)
case=ROOT/'dynamic-nonresident'
data=json.loads((case/'manifest.json').read_text())
receipt={}
with tempfile.TemporaryDirectory() as tmp:
    for member in data['members']:
        name=member['filename']
        image=Path(tmp)/name
        with lzma.open(case/(name+'.xz')) as z,image.open('xb') as out:
            shutil.copyfileobj(z,out,1<<20)
        saved=case/(name+'.guest.raw.xz')
        receipt[saved.name]=read_guest(image,member,saved)
(case/'driver.json').write_text(json.dumps(receipt,indent=2)+'\n',encoding='utf-8')
