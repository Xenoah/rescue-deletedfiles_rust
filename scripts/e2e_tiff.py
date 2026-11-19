"""Check TIFF/BigTIFF carving with real strip, tile and multi-page encoder outputs."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
from e2e import check, run

ROOT = Path(__file__).resolve().parents[1]


def samples():
    manifest = json.loads((ROOT/'tests/tiff/manifest.json').read_text())
    files = {name:(ROOT/'tests/tiff'/(name+'.tif')).read_bytes() for name in manifest}
    for name,data in files.items():
        check(hashlib.sha256(data).hexdigest()==manifest[name]['sha256'],name)
    files['corpus']=(ROOT/'tests/corpus/files/sample.tif').read_bytes()
    return files,manifest


def entries(data):
    # The offset edits below use only little-endian classic TIFF fixtures.
    check(data[:4]==b'II*\0','test input format')
    p=struct.unpack_from('<I',data,4)[0]
    count=struct.unpack_from('<H',data,p)[0]
    rows={struct.unpack_from('<H',data,p+2+12*i)[0]:p+2+12*i for i in range(count)}
    return rows,p+2+12*count


def verify(binary,root,independent=False):
    files,manifest=samples()
    disk=root/'pictures.raw';image=bytearray(b'\x85'*(1024*1024-1));offsets={}
    for name,b in files.items():
        offsets[len(image)]=name;image.extend(b);image.extend(b'\xf0\x80')
    disk.write_bytes(image)
    expected={name:hashlib.sha256(b).hexdigest() for name,b in files.items()}
    for mode, flags in [('session',[]),('index',['--indexed'])]:
        scan=run(binary,'scan','--source',disk,'--mode','carve','--extension','tiff' if mode=='index' else 'tif',
                 '--work-dir',root/mode,'--json',*flags)
        found=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(found)==len(files) and {c['record'] for c in found}==offsets.keys(),found)
        for c in found:
            name=offsets[c['record']]
            check(c['supported'] and c['size']==len(files[name]) and c['record_sha256']==expected[name],(name,c))
            row,=run(binary,'recover','--session',scan['session'],'--id',c['id'],'--destination',
                     root/(mode+'-out'),'--json')['results']
            output=Path(row['path']);check(row['sha256']==expected[name] and output.read_bytes()==files[name],row)
            if independent and name in manifest:
                rgb=subprocess.check_output(['convert',output,'-depth','8','rgb:-'])
                pages=subprocess.check_output(['identify','-format','%p\n',output],text=True).splitlines()
                check(hashlib.sha256(rgb).hexdigest()==manifest[name]['rgb_sha256'] and len(pages)==manifest[name]['pages'],name)
        print(mode,len(found),'TIFF images byte-exact',flush=True)
    b=files['corpus'];tags,next_at=entries(b)
    def put(original,pos,values):
        return original[:pos]+values+original[pos+len(values):]
    def field(tag,value):
        return put(b,tags[tag]+8,struct.pack('<I',value))
    z=files['deflate'];zt,_=entries(z)
    pixels=struct.unpack_from('<I',z,zt[273]+8)[0]
    negatives=[b[:-1],put(b,next_at,struct.pack('<I',8)),field(273,0),field(279,0),
               field(273,0xfffffff0),put(b,tags[284],struct.pack('<H',65000)),
               put(b,tags[257],struct.pack('<H',256)),put(b,tags[258]+4,struct.pack('<I',0xffffffff)),
               put(files['bigtiff'],4,b'\0\x04'),put(z,pixels,b'\0\0')]
    for i,data in enumerate(negatives):
        bad=root/f'broken-{i}.tif';bad.write_bytes(data)
        scan=run(binary,'scan','--source',bad,'--mode','carve','--extension','tif','--json',
                 '--work-dir',root/f'bad-{i}',expected=5)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(rows and all(not c['supported'] for c in rows),(i,rows))
    check(disk.read_bytes()==image,'source changed')
    return expected


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--independent',action='store_true')
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as d:
        verify(args.binary.resolve(),Path(d),args.independent)
