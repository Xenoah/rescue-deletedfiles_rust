"""Find unknown-position fragments using independent rolling and SHA-256 chunk manifests."""
import argparse
import concurrent.futures
import hashlib
import io
import json
from pathlib import Path
import random
import re
import struct
import subprocess
import tempfile
import wave
import zipfile

from e2e import check, run
from e2e_imaging import mark_gap
from e2e_raid import encode, save

BASE = 0x100000001b3
MASK = 2**64-1


def sha(data):
    return hashlib.sha256(data).hexdigest()


def manifest(data, size=512):
    chunks=[]
    for p in range(0,len(data),size):
        block=data[p:p+size]
        weak=0
        for v in block:weak=(weak*BASE+v)&MASK
        chunks.append(dict(rolling64=f'{weak:016x}',sha256=sha(block)))
    return dict(schema_version=1,chunk_size=size,file_bytes=len(data),sha256=sha(data),chunks=chunks)


def samples():
    rng=random.Random(102751)
    wav=io.BytesIO()
    with wave.open(wav,'wb') as f:
        f.setnchannels(1);f.setsampwidth(2);f.setframerate(8000)
        f.writeframes(b''.join(struct.pack('<h',(i*97)%65536-32768) for i in range(1500)))
    archive=io.BytesIO()
    with zipfile.ZipFile(archive,'w') as z:
        info=zipfile.ZipInfo('record.txt',(2020,1,2,3,4,6))
        z.writestr(info,b''.join(f'original {i}\n'.encode() for i in range(350)))
    return {'NOTE.TXT':('あいうえお\n'*265).encode(),'DATA.CSV':b'key,value\n'+b''.join(f'{i},{i*i}\n'.encode() for i in range(357)),
            'PAGE.HTM':b'<html><body>'+b''.join(f'<p>Paragraph {i}</p>'.encode() for i in range(200))+b'</body></html>',
            'SONG.WAV':wav.getvalue(),'FILES.ZIP':archive.getvalue(),'VENDOR.BIN':rng.randbytes(5901),
            'DUP.DAT':(bytes(range(256))*2)*9+b'end'}


def generate(binary, folder, name, data, size=512):
    src=folder/name;src.write_bytes(data)
    path=folder/(name+'.manifest.json')
    result=run(binary,'fingerprint-file','--source',src,'--output',path,'--chunk-size',str(size),'--json')
    oracle=manifest(data,size)
    check(json.loads(path.read_text())==oracle and result['sha256']==sha(data) and
          result['chunks']==len(oracle['chunks']) and result['manifest']['sha256']==sha(path.read_bytes()),name)
    return src,path


def recover(binary, disk, path, dest, *extra, expected=0):
    return run(binary,'recover-fingerprinted','--source',disk,'--manifest',path,
               '--output',dest,'--json',*extra,expected=expected)


def verify(binary, root):
    files=samples()
    files['EMPTY.BIN']=b''
    files['SHORT.BIN']=b'!2'
    files['ZEROS.BIN']=bytes(1024)
    disk=root/'anonymous.raw'
    rng=random.Random(8019)
    data=bytearray(rng.randbytes(1024*1024-11))
    entries={}
    for name,original in files.items():
        source,plan=generate(binary,root,name,original)
        entries[name]=plan
        source.unlink()
    # Cross the scanner's 1 MiB read-window boundary, reorder physical extents,
    # and store only one copy of repeated content. None of the positions is in a manifest.
    blocks=sorted({b[i:i+512] for b in files.values() for i in range(0,len(b),512)},reverse=True)
    rng.shuffle(blocks)
    for b in blocks:data.extend(b+rng.randbytes(19))
    disk.write_bytes(data)
    proofs={}
    for name,original in files.items():
        out=root/('restored-'+name)
        report=recover(binary,disk,entries[name],out)
        check(report['original_sha256_verified'] and out.read_bytes()==original and report['file']['sha256']==sha(original),name)
        expected=len(set((c['rolling64'],c['sha256']) for c in manifest(original)['chunks']))
        check(report['distinct_chunks']==expected and report['matched_chunks']==len(manifest(original)['chunks']),name)
        proofs[name]=sha(original)
    print(len(files),'original files recovered without positions',flush=True)
    plan=entries['VENDOR.BIN'];other=root/'reject.bin'
    recover(binary,disk,plan,other,'--start','0','--end','512',expected=4)
    recover(binary,disk,plan,other,'--alignment','512',expected=4)
    check(not other.exists(),'missing or unaligned content')
    for k,v,code in [('alignment','3',2),('end',str(len(data)+1),2)]:
        recover(binary,disk,plan,other,'--'+k,v,expected=code)
    for label,modify in [('digest',lambda d:d.update(sha256='a'*64)),
                         ('weak',lambda d:d['chunks'][0].update(rolling64='f'*16)),
                         ('strong',lambda d:d['chunks'][0].update(sha256='a'*64))]:
        bad=root/(label+'.json');v=json.loads(plan.read_text());modify(v);bad.write_text(json.dumps(v))
        recover(binary,disk,bad,other,expected=4);check(not other.exists(),label)
    for label,modify in [('version',lambda d:d.update(schema_version=2)),('size',lambda d:d.update(file_bytes=1)),
                         ('stride',lambda d:d.update(chunk_size=513)),('hex',lambda d:d['chunks'][0].update(rolling64='not hex')),
                         ('hash',lambda d:d['chunks'][0].update(sha256='short'))]:
        bad=root/(label+'.json');v=json.loads(plan.read_text());modify(v);bad.write_text(json.dumps(v))
        recover(binary,disk,bad,other,expected=2);check(not other.exists(),label)
    args=[str(binary),'recover-fingerprinted','--source',str(disk),'--manifest',str(plan),'--output',str(other),'--json']
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        codes=list(pool.map(lambda _:subprocess.run(args,capture_output=True).returncode,range(4)))
    check(codes.count(0)==1 and other.read_bytes()==files['VENDOR.BIN'],codes)
    trg=Path(str(disk)+'.rmap')
    recover(binary,disk,plan,trg,expected=4)
    check(not trg.exists(),'read-map alias')
    cfg,_=encode(root/'members',bytes(data),'raid0',n=2)
    arr=save(root,cfg);out=root/'rebuilt.wav'
    recover(binary,arr,entries['SONG.WAV'],out)
    check(out.read_bytes()==files['SONG.WAV'],'RAID')
    zero=root/'zero.raw';zero.write_bytes(bytes(8192))
    acquired=Path(run(binary,'image','--source',zero,'--destination',root/'acquisition','--block-size','512','--json')['image'])
    mark_gap(acquired,1024)
    out=root/'missing.bin'
    recover(binary,acquired,entries['ZEROS.BIN'],out,'--start','1024','--end','1536',expected=4)
    check(not out.exists(),'synthetic zeros admitted')
    recover(binary,acquired,entries['ZEROS.BIN'],out,'--start','1024','--end','2048')
    check(out.read_bytes()==files['ZEROS.BIN'],'real readable duplicate not found')
    check(disk.read_bytes()==data,'source changed')
    return proofs


def native(binary,root):
    def m(*argv):
        return subprocess.run(list(map(str,argv)),capture_output=True,check=True,timeout=90).stdout.decode()
    fat=root/'owned-floppy.img'
    with fat.open('wb') as f:f.truncate(1440*1024)
    m('mformat','-i',fat,'-f','1440','::')
    fillers=[]
    for i in range(160):
        p=root/f'F{i:03}.BIN';p.write_bytes(bytes([i])*1024);fillers.append(p)
    m('mcopy','-i',fat,*fillers,'::')
    listing=m('mdir','-i',fat,'::')
    free=int(re.sub(r'[^0-9]','',next(l for l in listing.splitlines() if 'bytes free' in l)))
    tail=root/'FILL.BIN'
    with tail.open('wb') as f:f.truncate(free-512)
    m('mcopy','-i',fat,tail,'::')
    m('mdel','-i',fat,*['::'+p.name for p in fillers[::2]])
    files=samples();plans={};layout={}
    for name,original in files.items():
        src,plans[name]=generate(binary,root,name,original)
        m('mcopy','-i',fat,src,'::'+name)
        layout[name]=m('mshowfat','-i',fat,'::'+name).strip()
        check(len(re.findall(r'<\d+(?:-\d+)?>',layout[name]))>=2,layout[name])
        src.unlink()
    m('mdel','-i',fat,*['::'+name for name in files])
    listing=m('mdir','-b','-i',fat,'::')
    check(all(name.lower() not in listing.lower() for name in files),'real unlink')
    before=sha(fat.read_bytes())
    for name,original in files.items():
        out=root/('restored-'+name)
        result=recover(binary,fat,plans[name],out,'--alignment','512')
        check(out.read_bytes()==original and result['file']['sha256']==sha(original),name)
    check(sha(fat.read_bytes())==before,'source changed')
    print('Seven real mtools unlinks, each independently confirmed fragmented by mshowfat, all recovered using hashes alone',flush=True)
    return {name:{'sha256':sha(data),'prior_allocation':layout[name]} for name,data in files.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',required=True,type=Path);p.add_argument('--native',action='store_true')
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as d:(native if a.native else verify)(a.binary.resolve(),Path(d))
