"""Capture real NTFS unlinks of seven multi-cluster documents and media in a two-child VHD."""
import argparse
import json
import lzma
from pathlib import Path
import shutil
import tempfile
from capture_driver_raw import read_guest
from generate_windows import ROOT, capture

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--flush-before-delete', action='store_true')
durable=parser.parse_args().flush_before_delete
label='dynamic-flushed' if durable else 'dynamic-nonresident'
with tempfile.TemporaryDirectory() as tmp:
    capture(Path(tmp), label, 'expandable', 2, layered=True, durable=durable)
case=ROOT/label
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
