"""Restore single bzip2, XZ and LZIP members and verify codec/trailer failures."""
import argparse
import bz2
import hashlib
import lzma
from pathlib import Path
import struct
import tempfile
import zlib

from e2e import check, run

CORPUS=Path(__file__).resolve().parents[1]/'tests'/'corpus'/'files'


def lzip(data):
    filters=[{'id':lzma.FILTER_LZMA1,'dict_size':1<<20,'lc':3,'lp':0,'pb':2}]
    raw=lzma.compress(data,format=lzma.FORMAT_RAW,filters=filters)
    check(lzma.decompress(raw,format=lzma.FORMAT_RAW,filters=filters)==data,'independent LZMA decode')
    return b'LZIP\x01\x14'+raw+struct.pack('<IQQ',zlib.crc32(data),len(data),len(raw)+26)


def verify(binary,root):
    data=bytes(range(256))*2048
    random=b''.join(hashlib.sha256(struct.pack('<I',i)).digest() for i in range(4096))
    files={'empty.bz2':bz2.compress(b''),'random.bz2':bz2.compress(random,compresslevel=1),
           'corpus.bz2':(CORPUS/'sample.bz2').read_bytes(),'empty.xz':lzma.compress(b''),
           'crc32.xz':lzma.compress(random,check=lzma.CHECK_CRC32),'sha256.xz':lzma.compress(data,check=lzma.CHECK_SHA256),
           'nocheck.xz':lzma.compress(data[:77],check=lzma.CHECK_NONE),'corpus.xz':(CORPUS/'sample.xz').read_bytes(),
           'empty.lz':lzip(b''),'random.lz':lzip(random),'compressible.lz':lzip(data)}
    content=bytearray(b'\xa5'*(1024*1024-3));offsets={}
    for name,b in files.items():
        offsets[len(content)]=name
        # Concatenated members, including a final member exactly at EOF.
        content+=b
    expected={name:hashlib.sha256(b).hexdigest() for name,b in files.items()}
    image=root/'streams.img';image.write_bytes(content)
    for label,flags in [('session',[]),('index',['--indexed'])]:
        scan=run(binary,'scan','--source',image,'--mode','carve','--extension','bz2','--extension','xz',
                 '--extension','lz','--json','--work-dir',root/label,*flags)
        found=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check({c['record'] for c in found}==offsets.keys(),found)
        for c in found:
            name=offsets[c['record']]
            check(c['supported'] and c['size']==len(files[name]) and c['record_sha256']==expected[name],(name,c))
            row,=run(binary,'recover','--session',scan['session'],'--id',c['id'],'--destination',root/(label+'-out'),'--json')['results']
            check(Path(row['path']).read_bytes()==files[name] and row['sha256']==expected[name],row)
        print(label,len(found),'members byte-exact')
    invalid={'cut.bz2':files['random.bz2'][:-4],'cut.xz':files['crc32.xz'][:-2],'cut.lz':files['random.lz'][:-8]}
    def flip(b,pos):
        result=bytearray(b);result[pos]^=1;return result
    invalid.update({'block-crc.bz2':flip(files['random.bz2'],10),'index.xz':flip(files['sha256.xz'],-14),
                    'footer.xz':flip(files['crc32.xz'],-11),'trailer-crc.lz':flip(files['random.lz'],-20),
                    'decoded-length.lz':flip(files['random.lz'],-16),'member-length.lz':flip(files['random.lz'],-8)})
    for i,(name,b) in enumerate(invalid.items()):
        image_bad=root/name;image_bad.write_bytes(b)
        scan=run(binary,'scan','--source',image_bad,'--mode','carve','--extension',Path(name).suffix[1:],
                 '--json','--work-dir',root/('bad'+str(i)),expected=5)
        found=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(found)==1 and found[0]['record']==0 and not found[0]['supported'],(name,found))
    check(image.read_bytes()==content,'source changed')
    return expected


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(p.parse_args().binary.resolve(),Path(tmp))
