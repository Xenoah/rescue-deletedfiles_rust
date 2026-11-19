"""Carve complete animations, including separate poster images and cropped/interlaced frames."""
import argparse
import hashlib
import io
from pathlib import Path
import struct
import tempfile
import zlib
from PIL import Image
from e2e import check, run
from make_carve_fixture import SIGNATURE, chunk

PASSES = [(0,0,8,8),(4,0,8,8),(0,4,4,8),(2,0,4,4),(0,2,2,4),(1,0,2,2),(0,1,1,2)]


def animation(poster=False, indexed=False, interlace=False, split=False, defect=None):
    width, height = 9, 8
    typ, depth = (3, 2) if indexed else (0, 8)
    chunks = [chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, depth, typ, 0, 0, int(interlace)))]
    if indexed:
        chunks.append(chunk(b'PLTE', bytes.fromhex('000000ff000000ff000000ff')))
    chunks.append(chunk(b'acTL', struct.pack('>II', 4 if defect == 'count' else 2, 0)))
    seq = 0

    def image_data(w, h, seed):
        rows = bytearray()
        for sx, sy, dx, dy in PASSES if interlace else [(0,0,1,1)]:
            if sx >= w:
                continue
            for y in range(sy, h, dy):
                rows.append(0)
                colors = [(x+y+seed) % (1 << depth) for x in range(sx, w, dx)]
                if indexed:
                    packed = bytearray((len(colors)*depth+7)//8)
                    for i, value in enumerate(colors):
                        packed[i//4] |= value << (6 - 2*(i%4))
                    rows.extend(packed)
                else:
                    rows.extend(colors)
        return zlib.compress(rows)

    def control(w, h, x, y, op=0):
        nonlocal seq
        chunks.append(chunk(b'fcTL', struct.pack('>IIIIIHHBB', seq, w, h, x, y, 1, 10, op, 0)))
        seq += 1

    def payload(data):
        nonlocal seq
        fragments = [data[:3], data[3:]] if split else [data]
        for frag in fragments:
            chunks.append(chunk(b'fdAT', struct.pack('>I', seq + int(defect == 'sequence')) + frag))
            seq += 1

    base = image_data(width,height,1)
    if not poster:
        control(width,height,0,0)
    chunks.append(chunk(b'IDAT',base))
    if poster:
        control(width,height,0,0)
        payload(image_data(width,height,2))
    control(4,3,8 if defect == 'bounds' else 1,2,3 if defect == 'dispose' else 1)
    data = image_data(4,3,3)
    if defect == 'trailing':
        data += zlib.compress(b'ignored')
    if defect == 'adler':
        data = data[:-1] + bytes([data[-1]^1])
    payload(data)
    chunks.append(chunk(b'IEND', b''))
    return SIGNATURE+b''.join(chunks)


def verify(binary,root):
    original = Path(__file__).resolve().parents[1]/'tests/corpus/files/sample.apng'
    variants = {'pillow':original.read_bytes(),'gray':animation(),'poster':animation(poster=True),
                'palette':animation(indexed=True),'split':animation(split=True),
                'adam7':animation(interlace=True),'palette-adam7':animation(indexed=True,interlace=True)}
    for name,data in variants.items():
        with Image.open(io.BytesIO(data)) as independent:
            # Pillow does not implement interlaced APNG subframe decompression.
            check(independent.n_frames >= 2, name)
            if 'adam7' not in name:
                for index in range(independent.n_frames):
                    independent.seek(index); independent.load()
    content=bytearray(1024*1024-2); offsets={}
    for name,data in variants.items():
        offsets[len(content)]=name
        content.extend(data+b'\xa5'*97)
    source=root/'animations.img';source.write_bytes(content)
    expected={name:hashlib.sha256(data).hexdigest() for name,data in variants.items()}
    for mode,flags,ext in [('session', [], 'png'), ('index', ['--indexed'], 'apng')]:
        result=run(binary,'scan','--source',source,'--mode','carve','--extension',ext,
                   '--work-dir',root/mode,'--json',*flags)
        candidates=run(binary,'results','--session',result['session'],'--json')['candidates']
        check({c['record'] for c in candidates}==offsets.keys(),candidates)
        for c in candidates:
            name=offsets[c['record']]
            check(c['name'].endswith('.apng') and c['extent_evidence']=='png_validated'
                  and c['record_sha256']==expected[name],(name,c))
            row,=run(binary,'recover','--session',result['session'],'--id',c['id'],
                     '--destination',root/(mode+'-out'),'--json')['results']
            check(Path(row['path']).read_bytes()==variants[name] and row['sha256']==expected[name],row)
        print(mode, len(candidates), 'animations byte-exact')
    for defect in ('sequence','bounds','count','dispose','trailing','adler'):
        damaged=root/(defect+'.raw');damaged.write_bytes(animation(defect=defect))
        result=run(binary,'scan','--source',damaged,'--mode','carve','--extension','apng',
                   '--work-dir',root/defect,'--json',expected=5)
        candidates=run(binary,'results','--session',result['session'],'--json')['candidates']
        check(len(candidates)==1 and not candidates[0]['supported'],(defect,candidates))
    check(source.read_bytes()==content,'source changed')
    return expected


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(p.parse_args().binary.resolve(),Path(tmp))
