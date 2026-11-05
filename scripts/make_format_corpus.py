"""Generate genuine, synthetic format fixtures using independent encoders/readers.

Optional generator dependencies are intentionally separate from the stdlib-only
recovery matrix runner. No user files or downloaded samples are used.
"""
import argparse
import bz2
import csv
from datetime import datetime, timezone
import gzip
import hashlib
import io
import json
import lzma
import platform
import sqlite3
import subprocess
import tarfile
from pathlib import Path
import zipfile
from PIL import Image, ImageOps
from reportlab.pdfgen import canvas
from docx import Document
from openpyxl import Workbook, load_workbook
from pptx import Presentation
import fitz

TEXT='Rescue cloud recovery corpus / 復元テスト / 0123456789\r\n'
FIXED=datetime(2026,1,1,tzinfo=timezone.utc)


def command(*args):
    return subprocess.run(list(map(str,args)),check=True,capture_output=True,text=True,encoding='utf-8',timeout=60).stdout


def make(root):
    files=root/'files';files.mkdir(parents=True,exist_ok=True)
    entries=[]
    def add(ext,label,category,writer,validator,reader):
        path=files/('sample.'+ext)
        writer(path);validator(path)
        data=path.read_bytes()
        entries.append({'format':label,'extension':ext,'category':category,'file':str(path.relative_to(root)).replace('\\','/'),
                        'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'validated_by':reader,
                        'carving_supported':ext in ('png','jpg')})
    image=Image.new('RGB',(32,32));image.putdata([(x*7%256,y*7%256,(x+y)*5%256) for y in range(32) for x in range(32)])
    def picture(path,fmt,**kw):image.save(path,format=fmt,**kw)
    def picture_valid(path):
        with Image.open(path) as loaded:
            loaded.load();assert loaded.size==(32,32)
    for ext,label,fmt in [('jpg','JPEG','JPEG'),('png','PNG','PNG'),('gif','GIF','GIF'),('bmp','BMP','BMP'),
                          ('tif','TIFF','TIFF'),('webp','WebP','WEBP'),('ico','ICO','ICO')]:
        add(ext,label,'image',lambda p,f=fmt:picture(p,f,**({'sizes':[(32,32)]} if f=='ICO' else {})),picture_valid,'Pillow decode')
    def apng(path):image.save(path,format='PNG',save_all=True,append_images=[ImageOps.mirror(image)],duration=100,loop=0)
    def apng_valid(path):
        with Image.open(path) as loaded:
            assert loaded.n_frames==2
            for i in range(2):loaded.seek(i);loaded.load()
    add('apng','APNG','image',apng,apng_valid,'Pillow two-frame decode')
    def pdf(path):
        c=canvas.Canvas(str(path),invariant=1);c.drawString(50,780,'Rescue recovery corpus');c.save()
    def pdf_valid(path):
        with fitz.open(path) as doc:assert len(doc)==1 and 'Rescue recovery corpus' in doc[0].get_text()
    add('pdf','PDF','document',pdf,pdf_valid,'PyMuPDF text extraction')
    def docx(path):
        doc=Document();doc.core_properties.created=FIXED;doc.core_properties.modified=FIXED
        doc.add_paragraph(TEXT);doc.save(path)
    add('docx','DOCX','document',docx,lambda p:assert_value('復元テスト' in Document(p).paragraphs[0].text),'python-docx reopen')
    def xlsx(path):
        book=Workbook();book.properties.created=FIXED;book.properties.modified=FIXED
        book.active.append(['Rescue','復元テスト',123]);book.save(path)
    def xlsx_valid(path):
        book=load_workbook(path);assert book.active['B1'].value=='復元テスト';book.close()
    add('xlsx','XLSX','document',xlsx,xlsx_valid,'openpyxl reopen')
    def pptx(path):
        slides=Presentation();slides.core_properties.created=FIXED;slides.core_properties.modified=FIXED
        slide=slides.slides.add_slide(slides.slide_layouts[1]);slide.shapes.title.text='Rescue recovery corpus';slide.placeholders[1].text='復元テスト'
        slides.save(path)
    add('pptx','PPTX','document',pptx,lambda p:assert_value(Presentation(p).slides[0].shapes.title.text=='Rescue recovery corpus'),'python-pptx reopen')
    add('txt','TXT (UTF-8)','text',lambda p:p.write_bytes(TEXT.encode()),lambda p:assert_value(p.read_bytes().decode()==TEXT),'UTF-8 decode')
    def csv_file(path):
        with path.open('w',encoding='utf-8',newline='') as f:csv.writer(f).writerows([['name','value'],['復元テスト','123']])
    add('csv','CSV (UTF-8)','text',csv_file,lambda p:assert_value(list(csv.reader(p.open(encoding='utf-8',newline='')))[1][0]=='復元テスト'),'csv reader')
    add('json','JSON','text',lambda p:p.write_text(json.dumps({'message':TEXT},ensure_ascii=False),encoding='utf-8'),lambda p:assert_value(json.loads(p.read_text(encoding='utf-8'))['message']==TEXT),'json parser')
    add('html','HTML','text',lambda p:p.write_text('<!doctype html><meta charset="utf-8"><title>Rescue</title><p>復元テスト</p>',encoding='utf-8'),lambda p:assert_value('復元テスト' in p.read_text(encoding='utf-8')),'UTF-8 document inspection')
    import xml.etree.ElementTree as ET
    add('xml','XML','text',lambda p:p.write_text('<?xml version="1.0" encoding="utf-8"?><rescue>復元テスト</rescue>',encoding='utf-8'),lambda p:assert_value(ET.parse(p).getroot().text=='復元テスト'),'ElementTree parse')
    add('rtf','RTF','document',lambda p:p.write_bytes(b'{\\rtf1\\ansi Rescue recovery corpus\\par byte-exact test}'),lambda p:assert_value(p.read_bytes().startswith(b'{\\rtf1')),'RTF header/structure inspection')
    for ext,label,codec in [('mp4','MP4','libx264'),('mov','MOV','libx264'),('avi','AVI','mpeg4'),('mkv','MKV','libx264'),('webm','WebM','libvpx-vp9')]:
        def video(path,codec=codec):
            command('ffmpeg','-nostdin','-v','error','-f','lavfi','-i','testsrc=size=32x32:rate=5','-t','0.6','-c:v',codec,'-pix_fmt','yuv420p','-threads','1','-y',path)
        add(ext,label,'video',video,media_valid,'FFmpeg full decode + FFprobe')
    for ext,label,codec in [('mp3','MP3','libmp3lame'),('wav','WAV','pcm_s16le'),('flac','FLAC','flac'),('ogg','Ogg/Vorbis','libvorbis'),('aac','AAC/ADTS','aac'),('m4a','M4A/AAC','aac')]:
        def audio(path,codec=codec):
            command('ffmpeg','-nostdin','-v','error','-f','lavfi','-i','sine=frequency=440:sample_rate=16000','-t','0.2','-c:a',codec,'-threads','1','-y',path)
        add(ext,label,'audio',audio,media_valid,'FFmpeg full decode + FFprobe')
    payload=TEXT.encode()
    def zip_file(path):
        with zipfile.ZipFile(path,'w',zipfile.ZIP_DEFLATED) as z:z.writestr('proof.txt',payload)
    def zip_valid(path):
        with zipfile.ZipFile(path) as z:assert z.testzip() is None and z.read('proof.txt')==payload
    add('zip','ZIP','archive',zip_file,zip_valid,'zipfile CRC + contents')
    def tar_file(path):
        with tarfile.open(path,'w') as tar:
            info=tarfile.TarInfo('proof.txt');info.size=len(payload);info.mtime=0;tar.addfile(info,io.BytesIO(payload))
    def tar_valid(path):
        with tarfile.open(path) as tar:assert tar.extractfile('proof.txt').read()==payload
    add('tar','TAR','archive',tar_file,tar_valid,'tarfile contents')
    for ext,label,compress,decompress in [('gz','GZIP',lambda b:gzip.compress(b,mtime=0),gzip.decompress),('xz','XZ',lzma.compress,lzma.decompress),('bz2','BZIP2',bz2.compress,bz2.decompress)]:
        add(ext,label,'archive',lambda p,c=compress:p.write_bytes(c(payload)),lambda p,d=decompress:assert_value(d(p.read_bytes())==payload),'stdlib decompression')
    def database(path):
        if path.exists():path.unlink()
        with sqlite3.connect(path) as db:db.execute('create table proof(value text)');db.execute('insert into proof values (?)',(TEXT,))
    def database_valid(path):
        with sqlite3.connect(path) as db:assert db.execute('pragma integrity_check').fetchone()[0]=='ok' and db.execute('select value from proof').fetchone()[0]==TEXT
    add('sqlite','SQLite','database',database,database_valid,'SQLite integrity_check + query')
    import random
    random_bytes=random.Random(20261104).randbytes(64*64*3)
    noisy=Image.frombytes('RGB',(64,64),random_bytes)
    noise_path=files/'noise.png';noisy.save(noise_path)
    with Image.open(noise_path) as loaded:loaded.load();assert loaded.size==(64,64)
    data=noise_path.read_bytes()
    entries.append({'format':'PNG','extension':'png','category':'image','file':'files/noise.png','bytes':len(data),
                    'sha256':hashlib.sha256(data).hexdigest(),'validated_by':'Pillow 64x64 noise decode','carving_supported':True})
    assert len(entries)==36 and len({entry['format'] for entry in entries})==35

    metadata={'schema_version':1,'provenance':'Generated synthetic content only; independent encoders/readers. No user files.',
              'python':platform.python_version(),'pillow':Image.__version__,'ffmpeg':command('ffmpeg','-version').splitlines()[0],
              'files':entries}
    (root/'manifest.json').write_text(json.dumps(metadata,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'formats':len({entry['format'] for entry in entries}),'files':len(entries),'bytes':sum(e['bytes'] for e in entries),'largest':max(e['bytes'] for e in entries)},indent=2))


def assert_value(value):assert value


def media_valid(path):
    command('ffmpeg','-nostdin','-v','error','-i',path,'-f','null','-')
    result=json.loads(command('ffprobe','-v','error','-show_streams','-of','json',path))
    assert result['streams']


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--output',type=Path,default=Path('tests/corpus'))
    make(parser.parse_args().output)
