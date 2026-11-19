"""Compare Android sparse imports to independent logical bytes and retain unknown DONT_CARE ranges."""
import argparse
import concurrent.futures
import ctypes
import hashlib
import json
import lzma
import os
import shutil
import struct
import subprocess
import tempfile
import zlib
from pathlib import Path

from e2e import check, run
from e2e_extended_filesystems import make_deleted
from e2e_html import make_deleted_fat

GOLD = Path(__file__).resolve().parents[1]/'tests/corpus/files'
REFERENCE = GOLD.parent.parent/'android-sparse'
FORMATS = ['bmp','gif','png','pdf','webp','wav','zip']
RAW, FILL, UNKNOWN, CRC = range(0xcac1,0xcac5)
MAGIC = 0xed26ff3a


def originals():
    return {p.name:p.read_bytes() for p in (GOLD/('sample.'+suffix) for suffix in FORMATS)}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pack_chunk(kind, blocks, data, extra=0):
    return struct.pack('<HHII',kind,0,blocks,12+extra+len(data))+bytes(extra)+data


def pack_header(block,blocks,chunks,digest=0,extra=0):
    return struct.pack('<IHHHHIIII',MAGIC,1,0,28+extra,12+extra,block,blocks,chunks,digest)+bytes(extra)


def invoke(binary,image,destination,expected=0):
    proc=subprocess.run([binary,'import-android','--source',image,'--destination',destination,'--json'],
                        text=True,capture_output=True,env=dict(os.environ,PATH=''),timeout=120)
    check(proc.returncode==expected,(proc.returncode,proc.stdout,proc.stderr))
    return json.loads(proc.stdout) if not expected else proc.stderr


def inspect(binary,report,reference,stored,unknown,checks):
    image=Path(report['path'])
    detail=report['android_sparse']
    check(report['bytes']==len(reference) and image.read_bytes()==reference and
          report['sha256']==sha(reference) and detail['input_sha256']==sha(stored),report)
    check(Path(detail['read_map']).samefile(str(image)+'.rmap'),detail)
    info=run(binary,'read-map','--image',image,'--json')
    check(info['copied_bytes']==detail['recorded_bytes']==len(reference)-unknown and
          info['unattempted_bytes']==detail['unrecorded_bytes']==unknown and info['failed_bytes']==0 and
          detail['crc32_checks']==checks, (info,detail))
    return image


def carve(binary,image,folder,files,has_gaps,indexed=False):
    flags=[arg for ext in FORMATS for arg in ('--extension',ext)]
    scan=run(binary,'scan','--source',image,'--mode','carve','--work-dir',folder/'scan',
             '--json',*flags,*(['--indexed'] if indexed else []),expected=5 if has_gaps else 0,timeout=300)
    found=run(binary,'results','--session',scan['session'],'--json')['candidates']
    expected={sha(data):name for name,data in files.items()}
    rows={row['record_sha256']:row for row in found if row.get('record_sha256') in expected and row['supported']}
    check(rows.keys()==expected.keys(),(folder,list(rows),list(expected)))
    ids=[v for row in rows.values() for v in ('--id',row['id'])]
    saved=run(binary,'recover','--session',scan['session'],*ids,'--destination',folder/'files','--json')['results']
    check(len(saved)==len(files),saved)
    for row in saved:
        data=Path(row['path']).read_bytes()
        check(data==files[expected[row['sha256']]] and sha(data)==row['sha256'],row)
    return {n:sha(d) for n,d in files.items()}


def portable(binary,root):
    files=originals()
    block=1024;extra=8
    payload=b''.join(data+bytes(block-len(data)%block) for data in files.values())
    pattern=bytes.fromhex('1245dbcf')
    a=payload+pattern*512
    logical=a+bytes(2*block)
    chunks=[pack_chunk(RAW,len(payload)//block,payload,extra),
            pack_chunk(FILL,2,pattern,extra),
            pack_chunk(CRC,0,struct.pack('<I',zlib.crc32(a)),extra),
            pack_chunk(UNKNOWN,1,b'',extra),pack_chunk(FILL,1,bytes(4),extra),
            pack_chunk(CRC,0,struct.pack('<I',zlib.crc32(logical)),extra)]
    header=pack_header(block,len(logical)//block,len(chunks),zlib.crc32(logical),extra)
    disk=root/'system.simg';stored=header+b''.join(chunks);disk.write_bytes(stored)
    dest=root/'verified'
    report=invoke(binary,disk,dest)
    image=inspect(binary,report,logical,stored,block,3)
    evidence=carve(binary,image,root/'portable',files,True)
    # Identical all-zero bytes on each side of this boundary have different provenance.
    def check_zero(offset,out,code):
        return run(binary,'recover-by-hash','--source',image,'--output',out,'--sha256',sha(bytes(block)),
                   '--file-bytes',block,'--first-offset',offset,'--start',offset,'--end',offset+block,'--json',expected=code)
    out=root/'unknown';check_zero(len(a),out,4);check(not out.exists(),'unknown source counted as zero')
    check_zero(len(a)+block,out,0);check(out.read_bytes()==bytes(block),'explicit zero fill')
    msg=run(binary,'import-image','--source',disk,'--output',root/'unsafe.raw','--json',expected=2)
    check('import-android' in str(msg) and not (root/'unsafe.raw').exists(),msg)
    invoke(binary,disk,dest,2);check(image.read_bytes()==logical,'existing directory overwritten')
    def changed(at,value):
        b=bytearray(stored);b[at:at+len(value)]=value;return b
    cases={
        'magic':changed(0,b'nope'), 'version':changed(4,b'\x02'), 'minor':changed(6,b'\x01'),
        'file-header':changed(8,b'\x04'), 'chunk-header':changed(10,b'\x05'), 'block':changed(12,struct.pack('<I',7)),
        'length':changed(16,struct.pack('<I',len(logical)//block+1)), 'zero-count':changed(20,bytes(4)),
        'chunk-limit':changed(20,struct.pack('<I',262145)), 'logical-limit':changed(16,struct.pack('<I',0xffffffff)),
        'header-crc':changed(24,struct.pack('<I',1)), 'reserved':changed(len(header)+2,b'\xfe'),
        'type':changed(len(header),b'\xa0'), 'payload-length':changed(len(header)+8,bytes(4)),
        'short':stored[:-1], 'trailing':stored+b'X', 'modified-data':changed(len(header)+12+extra,b'\xff'),
        'missing-chunk':header+b''.join(chunks[:-2]), 'final-crc':stored[:-4]+bytes(4),
    }
    for n,b in cases.items():
        path=root/(n+'.simg');path.write_bytes(b);target=root/('out-'+n)
        invoke(binary,path,target,4);check(not target.exists(),(n,target))
    # A concurrent publisher cannot replace even an existing empty directory.
    empty=root/'reserved';empty.mkdir();invoke(binary,disk,empty,2);check(not list(empty.iterdir()),empty)
    racing=root/'concurrent'
    def race(_):
        return subprocess.run([binary,'import-android','--source',disk,'--destination',racing],capture_output=True).returncode
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        codes=list(pool.map(race,range(3)))
    check(codes.count(0)==1 and (racing/'image.rescue.raw').read_bytes()==logical,codes)
    check(disk.read_bytes()==stored,'source changed')
    print('Android sparse RAW/FILL/DONT_CARE, padded headers, intermediate/final/header CRCs, '
          'seven exact files, recorded zeros versus unknowns, 19 malformed inputs and concurrent publication passed',flush=True)
    frozen(binary,root)
    return evidence


def command(*args):
    return subprocess.run(list(map(str,args)),capture_output=True,check=True,timeout=180)


def aosp_with_omitted_tail(raw,simg,original,block):
    # Jammy's AOSP img2simg predates -s. Use the same upstream library to
    # explicitly record the occupied prefix and omit a known zero tail.
    ldd=command('ldd',shutil.which('img2simg')).stdout.decode()
    library=next(line.split('=>',1)[1].strip().split()[0] for line in ldd.splitlines()
                 if line.strip().startswith('libsparse.so') and '=>' in line)
    lib=ctypes.CDLL(library)
    lib.sparse_file_new.argtypes=[ctypes.c_uint,ctypes.c_int64]
    lib.sparse_file_new.restype=ctypes.c_void_p
    lib.sparse_file_add_fd.argtypes=[ctypes.c_void_p,ctypes.c_int,ctypes.c_int64,ctypes.c_uint,ctypes.c_uint]
    lib.sparse_file_write.argtypes=[ctypes.c_void_p,ctypes.c_int,ctypes.c_bool,ctypes.c_bool,ctypes.c_bool]
    lib.sparse_file_destroy.argtypes=[ctypes.c_void_p]
    end=((len(original.rstrip(b'\x00'))+block-1)//block)*block
    check(0<end<len(original) and not any(original[end:]),'expected empty suffix')
    sparse=lib.sparse_file_new(block,len(original))
    check(sparse,'AOSP sparse_file_new')
    try:
        with raw.open('rb') as src,simg.open('wb') as dest:
            check(lib.sparse_file_add_fd(sparse,src.fileno(),0,end,0)==0,'AOSP add fd')
            check(lib.sparse_file_write(sparse,dest.fileno(),False,True,False)==0,'AOSP write')
    finally:
        lib.sparse_file_destroy(sparse)


def record_reference(stage,destination):
    # A real FAT32 deletion set wrapped with AOSP's SEEK_HOLE-aware encoder.
    # The independent simg2img expansion must equal the original full volume.
    files=originals()
    raw=make_deleted_fat(stage,files)
    source=raw.read_bytes()
    simg=stage/'reference.simg'
    command('img2simg','-s',raw,simg,4096)
    baseline=stage/'android.raw'
    command('simg2img',simg,baseline)
    check(baseline.read_bytes()==source,'AOSP reference round trip')
    stored=simg.read_bytes()
    fh,ch=struct.unpack_from('<HH',stored,8)
    block,_,count=struct.unpack_from('<III',stored,12)
    pos=fh;unknown=0
    for _ in range(count):
        tag,_,n,length=struct.unpack_from('<HHII',stored,pos)
        if tag==UNKNOWN:unknown+=n*block
        pos+=length
    check(pos==len(stored) and unknown>0,'reference lacks omitted ranges')
    destination.mkdir(parents=True,exist_ok=True)
    bundle=lzma.compress(stored)
    (destination/'fat32.simg.xz').write_bytes(bundle)
    manifest=dict(producer='AOSP img2simg -s (Android platform tools 34.0.5)',logical_bytes=len(source),
                  logical_sha256=sha(source),sparse_sha256=sha(stored),xz_sha256=sha(bundle),
                  block_size=block,unrecorded_bytes=unknown,originals={n:sha(b) for n,b in files.items()})
    (destination/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')


def frozen(binary,stage):
    original=json.loads((REFERENCE/'manifest.json').read_text())
    bundle=(REFERENCE/'fat32.simg.xz').read_bytes()
    stored=lzma.decompress(bundle)
    check(sha(bundle)==original['xz_sha256'] and sha(stored)==original['sparse_sha256'],'reference damaged')
    source=stage/'aosp.simg';source.write_bytes(stored)
    report=invoke(binary,source,stage/'aosp')
    raw=Path(report['path'])
    info=report['android_sparse']
    check(report['bytes']==original['logical_bytes'] and sha(raw.read_bytes())==report['sha256']==original['logical_sha256'],report)
    check(info['input_sha256']==original['sparse_sha256'] and info['unrecorded_bytes']==original['unrecorded_bytes'],info)
    for indexed in [False,True]:
        result=carve(binary,raw,stage/('index' if indexed else 'session'),originals(),True,indexed)
        check(result==original['originals'],result)
    print('AOSP reference with seven actual FAT32 unlinks recovered twice, exact bytes and hashes in session and index',flush=True)
    return original


def native(binary,root):
    files=originals()
    proof={}
    for filesystem,block,detect_holes in [('fat32',1024,False),('ext4',4096,False),('fat32-holes',4096,True)]:
        stage=root/filesystem;stage.mkdir()
        if filesystem.startswith('fat32'):
            raw=make_deleted_fat(stage,files)
        else:
            for name,data in files.items():
                (stage/name).write_bytes(data)
            raw=make_deleted(stage,'ext4',files)
        original=raw.read_bytes();simg=stage/'android.simg'
        if detect_holes:
            aosp_with_omitted_tail(raw,simg,original,block)
        else:
            command('img2simg',raw,simg,block)
        baseline=stage/'aosp.raw';command('simg2img',simg,baseline)
        check(baseline.read_bytes()==original,'AOSP reconstruction')
        stored=simg.read_bytes()
        # Independent chunk walk to compare recorded vs unrecorded byte counts.
        fh,ch=struct.unpack_from('<HH',stored,8)
        chunks=struct.unpack_from('<I',stored,20)[0]
        pos=fh;unknown=0
        for _ in range(chunks):
            tag,_,n,bytes_in=struct.unpack_from('<HHII',stored,pos)
            if tag==UNKNOWN:unknown+=n*block
            pos+=bytes_in
        check(pos==len(stored) and (unknown>0 if detect_holes else unknown==0),(filesystem,unknown))
        report=invoke(binary,simg,stage/'mapped')
        image=inspect(binary,report,baseline.read_bytes(),stored,unknown,0)
        rows=carve(binary,image,stage,files,bool(unknown))
        check(raw.read_bytes()==original and simg.read_bytes()==stored,'input changed')
        proof[filesystem]=dict(originals=rows,logical_sha256=sha(original),unrecorded_bytes=unknown)
        print(filesystem,block,'AOSP encoded and independently expanded, seven actual deletions byte-exact,',unknown,'bytes unrecorded',flush=True)
    return proof


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path)
    parser.add_argument('--write-reference',type=Path)
    parser.add_argument('--native',action='store_true')
    parser.add_argument('--evidence',type=Path)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        if args.write_reference:
            record_reference(Path(temp),args.write_reference)
            raise SystemExit(0)
        if not args.binary:parser.error('--binary is required for verification')
        proof=(native if args.native else portable)(args.binary.resolve(),Path(temp))
    if args.evidence:args.evidence.write_text(json.dumps(proof,indent=2)+'\n')
