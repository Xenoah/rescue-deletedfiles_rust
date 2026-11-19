"""Recover ICO and CUR image directories with independent DIB and PNG resources."""
import argparse
import hashlib
import io
from pathlib import Path
import struct
import tempfile
from PIL import Image
from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus' / 'files'


def dib(w,h,bits,palette=None):
    colors = (1 << bits) if bits <= 8 and palette is None else (palette or 0)
    row=((w*bits+31)//32)*4
    mask=((w+31)//32)*4
    header=struct.pack('<IiiHHIIiiII',40,w,h*2,1,bits,0,0,0,0,colors,0)
    table=b''.join(bytes([k,255-k,k//2,0]) for k in range(colors))
    return header+table+bytes(row*h)+bytes(mask*h)


def directory(kind, entries):
    start=6+16*len(entries)
    # Name order in an icon directory does not define physical resource order.
    records=[]
    data=b''
    for w,h,bits,body in reversed(entries):
        a,b=(1,bits) if kind==1 else (w-1,h-1)
        records.append(struct.pack('<BBBBHHII', w%256,h%256,0,0,a,b,len(body),start+len(data)))
        data+=body
    return struct.pack('<HHH',0,kind,len(entries))+b''.join(reversed(records))+data


def verify(binary, root):
    original=(CORPUS/'sample.ico').read_bytes()
    png=original[22:]
    files={
        'png.ico':original,
        'mono.ico':directory(1,[(15,5,1,dib(15,5,1))]),
        'palette.ico':directory(1,[(29,17,4,dib(29,17,4,7))]),
        'padded.ico':directory(1,[(7,19,24,dib(7,19,24))]),
        'multi.ico':directory(1,[(32,32,32,png),(16,13,32,dib(16,13,32))]),
        'hotspot.cur':directory(2,[(8,14,24,dib(8,14,24))]),
        'mixed.cur':directory(2,[(32,32,32,png),(12,9,4,dib(12,9,4))]),
    }
    with Image.open(io.BytesIO(original)) as image:
        image.load(); check(image.size==(32,32),'independent icon decoder')
    source=root/'icons.raw';offsets={};raw=bytearray(1024*1024-17)
    for name,data in files.items():
        offsets[len(raw)]=name;raw.extend(data+b'after'*9)
    source.write_bytes(raw)
    exact={name:hashlib.sha256(data).hexdigest() for name,data in files.items()}
    for label,flags in [('session',[]),('index',['--indexed'])]:
        result=run(binary,'scan','--mode','carve','--source',source,'--extension','ico','--extension','cur',
                   '--work-dir',root/label,'--json',*flags)
        found=run(binary,'results','--session',result['session'],'--json')['candidates']
        check({c['record'] for c in found if c['supported']}==offsets.keys(),found)
        for c in found:
            if not c['supported']:
                continue
            name=offsets[c['record']]
            check(c['record_sha256']==exact[name] and Path(c['name']).suffix==Path(name).suffix,c)
            row,=run(binary,'recover','--session',result['session'],'--id',c['id'],'--destination',root/(label+'-out'),'--json')['results']
            check(Path(row['path']).read_bytes()==files[name] and row['sha256']==exact[name],row)
        print(label,len(files),'icons/cursors byte-exact')
    def patched(b,at,value):
        return b[:at]+value+b[at+len(value):]
    corrupt = [original[:-1], patched(original,6,b'\x1f'), patched(original,len(original)-1,b'\x00'),
               patched(files['hotspot.cur'],10,b'\xff\xff'), patched(files['padded.ico'],22+8,struct.pack('<I',19)),
               patched(files['palette.ico'],22+40+7*4,b'\xff')]
    for i,data in enumerate(corrupt):
        path=root/(str(i)+'.raw');path.write_bytes(data)
        r=run(binary,'scan','--mode','carve','--source',path,'--extension','ico','--extension','cur',
              '--work-dir',root/('invalid'+str(i)),'--json',expected=5)
        f=run(binary,'results','--session',r['session'],'--json')['candidates']
        check(any(c['record']==0 for c in f) and not any(c['supported'] for c in f),i)
    check(source.read_bytes()==raw,'source changed')
    return exact


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(p.parse_args().binary.resolve(),Path(tmp))
