"""Verify standalone SFNT and WOFF recovery against originals and independently render glyphs."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile

from e2e import check, run

FIXTURES = Path(__file__).resolve().parents[1] / 'tests/fonts'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def verify(binary, root, independent=False, system_fonts=()):
    manifest = json.loads((FIXTURES / 'manifest.json').read_text())
    files = {name:(FIXTURES/name).read_bytes() for name in manifest}
    for name,data in files.items():
        check(digest(data)==manifest[name]['sha256'],name)
    for i,path in enumerate(system_fonts):
        files[f'installed-{i}{path.suffix.lower()}']=path.read_bytes()
    raw=root/'typefaces.raw'
    content=bytearray(b'\xaf'*(1024*1024-6))
    positions={}
    for name,data in files.items():
        positions[len(content)]=name
        content.extend(data+b'\x55'*9)
    raw.write_bytes(content)
    flags=['--extension','ttf','--extension','otf','--extension','woff']
    for mode,index in [('session',[]),('indexed',['--indexed'])]:
        scan=run(binary,'scan','--source',raw,'--mode','carve','--work-dir',root/mode,'--json',*flags,*index)
        results=run(binary,'results','--session',scan['session'],'--json')['candidates']
        found={r['record']:r for r in results if r['supported']}
        check(found.keys()==positions.keys(),{'missing':[positions[k] for k in positions.keys()-found.keys()],
                                              'unexpected':list(found.keys()-positions.keys())})
        for offset,row in found.items():
            name=positions[offset]
            check(row['name'].endswith(Path(name).suffix) and row['size']==len(files[name]) and
                  row['record_sha256']==digest(files[name]),row)
            recovered,=run(binary,'recover','--session',scan['session'],'--id',row['id'],
                           '--destination',root/(mode+'-output'),'--json')['results']
            data=Path(recovered['path']).read_bytes()
            check(data==files[name] and recovered['sha256']==digest(files[name]),name)
            if independent:
                from fontTools.ttLib import TTFont
                from PIL import ImageFont
                font=TTFont(io.BytesIO(data),checkChecksums=2)
                cmap=font.getBestCmap()
                if name in manifest:
                    check(font['name'].getDebugName(1)==manifest[name]['family'] and sorted(cmap)==manifest[name]['characters'],name)
                font.flavor=None;sfnt=io.BytesIO();font.save(sfnt);sfnt.seek(0)
                rendered=ImageFont.truetype(sfnt,32)
                check(rendered.getmask('A').getbbox() is not None,name)
        print(mode,len(found),'complete font files matched',flush=True)

    # Both a CFF OpenType font and an OpenDocument formula template use .otf.
    from e2e_open_documents import samples
    template = next(data for name,data in samples().items() if name.endswith('.otf'))
    paired = root/'different-types.raw'
    paired.write_bytes(template + bytes(512) + files['otf-1.otf'])
    for i, index in enumerate([[], ['--indexed']]):
        scan=run(binary,'scan','--source',paired,'--mode','carve','--extension','otf',
                 '--work-dir',root/f'pair-{i}','--json',*index)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check({r['id'].split('-')[0] for r in rows}=={'otf','opentype'} and len(rows)==2,rows)
        for row in rows:
            result,=run(binary,'recover','--session',scan['session'],'--id',row['id'],
                        '--destination',root/f'pair-out-{i}','--json')['results']
            expected=template if row['id'].startswith('otf-') else files['otf-1.otf']
            check(Path(result['path']).read_bytes()==expected,row)

    def replace(b,at,fmt,v):
        copy=bytearray(b);struct.pack_into('>'+fmt,copy,at,v);return bytes(copy)
    sfnt=files['ttf-5.ttf'];woff=files['ttf-5.woff']
    count=int.from_bytes(sfnt[4:6],'big')
    entries=[struct.unpack_from('>4sIII',sfnt,12+i*16) for i in range(count)]
    head=next(e for e in entries if e[0]==b'head')
    tagged=next(e for e in entries if e[0]==b'TEST')
    def flip(b,at):return b[:at]+bytes([b[at]^1])+b[at+1:]
    packed_at=next(struct.unpack_from('>4sIIII',woff,44+i*20)[1] for i in range(int.from_bytes(woff[12:14],'big'))
                   if struct.unpack_from('>4sIIII',woff,44+i*20)[2]<struct.unpack_from('>4sIIII',woff,44+i*20)[3])
    invalid=[sfnt[:-4],flip(sfnt,head[2]+8),flip(sfnt,tagged[2]),flip(sfnt,16),
             replace(sfnt,20,'I',12),replace(sfnt,24,'I',0xffffffff),replace(sfnt,12+16+8,'I',entries[0][2]),
             sfnt[:28]+sfnt[12:16]+sfnt[32:],
             woff[:-1],replace(woff,14,'H',1),replace(woff,16,'I',0xffffffff),replace(woff,44+4,'I',44),
             replace(woff,44+8,'I',0xffffffff),flip(woff,44+16),flip(woff,packed_at),
             replace(woff,24,'I',48),replace(woff,32,'I',0xffffffff),replace(woff,36,'I',0),
             replace(woff,40,'I',0xffffffff)]
    for i,data in enumerate(invalid):
        image=root/f'bad-{i}.img';image.write_bytes(data)
        scan=run(binary,'scan','--source',image,'--mode','carve','--work-dir',root/f'bad-{i}','--json',*flags,expected=5)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(rows and all(not c['supported'] for c in rows),(i,rows))
    check(raw.read_bytes()==content,'source changed')
    print(len(invalid),'invalid font layouts and checksums refused',flush=True)
    return {name:digest(b) for name,b in files.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--independent',action='store_true');p.add_argument('--system-font',type=Path,action='append',default=[])
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(a.binary.resolve(),Path(tmp),a.independent,a.system_font)
