"""Reproducible UI demo: real corpus bytes, synthetic NTFS folders and dates.

Dates and folder names are explicit fixture values, not physical-device evidence.
Only creates a new regular image at the requested path.
"""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path
import struct
from e2e_formats import corpus_load, ntfs_image
from make_fixture import MFT_LCN, SECTOR, RECORD, filename, record, resident, put


def build(image):
    corpus=list(corpus_load(Path(__file__).resolve().parent.parent/'tests/corpus'))
    truth,_=ntfs_image(image,corpus,'fragmented')
    groups=[('01_書類',{'pdf','docx','xlsx','pptx','txt','csv','html','xml','rtf'}),('02_画像',{'jpg','jpeg','png','gif','bmp','tif','tiff','webp','ico','apng'}),('03_動画',{'mp4','mov','avi','mkv','webm'}),('04_音声',{'mp3','wav','flac','ogg','aac','m4a'}),('05_アーカイブ',{'zip','tar','gz','xz','bz2'}),('06_データ',{'sqlite','json'})]
    dates={}
    with image.open('r+b') as f:
        for index,(folder,_) in enumerate(groups):
            f.seek(MFT_LCN*SECTOR+(80+index)*RECORD);f.write(record(80+index,[filename(folder)],flags=3,sequence=1))
        for index,(key,entry) in enumerate(truth.items()):
            n=int(key.split('-')[1]);offset=MFT_LCN*SECTOR+n*RECORD;f.seek(offset);raw=bytearray(f.read(RECORD));usa,count=struct.unpack_from('<HH',raw,4)
            for i in range(1,count):raw[i*SECTOR-2:i*SECTOR]=raw[usa+i*2:usa+i*2+2]
            attrs=[];at=struct.unpack_from('<H',raw,20)[0]
            while struct.unpack_from('<I',raw,at)[0]!=0xffffffff:
                kind,length=struct.unpack_from('<II',raw,at)
                if kind!=0x30:attrs.append(raw[at:at+length])
                at+=length
            ext=entry['path'].suffix[1:].lower();group=next(i for i,(_,extensions) in enumerate(groups) if ext in extensions)
            created=datetime(2026,10,11,8,38,tzinfo=timezone.utc)+timedelta(days=index%7,minutes=index)
            modified=created+timedelta(minutes=index+1)
            si=bytearray(72)
            for at,value in [(0,created),(8,modified)]:put(si,at,'Q',(int(value.timestamp())+11644473600)*10_000_000)
            updated=[resident(0x10,si),filename(entry['path'].name,parent=80+group),*attrs]
            f.seek(offset);f.write(record(n,updated))
            dates[key]={'created':created.isoformat().replace('+00:00','Z'),'modified':modified.isoformat().replace('+00:00','Z')}
    return {'image_sha256':hashlib.sha256(image.read_bytes()).hexdigest(),'scope':'synthetic NTFS metadata with real 35-format corpus bytes; dates are fixture values','files':{key:{'sha256':entry['sha256'],'bytes':entry['bytes'],**dates[key]} for key,entry in truth.items()}}

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('image',type=Path);p.add_argument('--manifest',type=Path,required=True);a=p.parse_args();a.manifest.write_text(json.dumps(build(a.image),ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
