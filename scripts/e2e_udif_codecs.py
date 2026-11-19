"""Verify modern UDIF runs against independently encoded LZFSE and XZ streams."""
import argparse
import json
import lzma
from pathlib import Path
import struct
import tempfile
from e2e import check, run
from e2e_udif import CORPUS, GOLDEN, END, assemble, crc_header, digest, file_hash, restore_files

LZFSE = 0x80000007
LZMA = 0x80000008


def originals():
    files = {'sample.'+ext:(CORPUS/('sample.'+ext)).read_bytes()
             for ext in ('png','mp3','xml','flac','tif','rtf','webp')}
    payload = bytes(512) + bytes(283).join(files.values())
    return payload.ljust((len(payload)+511)//512*512,b'\0'), {k:digest(v) for k,v in files.items()}


def wrapped(data, codec, encoded):
    sectors=len(data)//512
    table=bytearray(204)
    struct.pack_into('>4sIQQQ',table,0,b'mish',1,0,sectors,0)
    table[64:76]=crc_header(data)
    struct.pack_into('>I',table,200,2)
    table+=struct.pack('>IIQQQQ',codec,0,0,sectors,0,len(encoded))
    table+=struct.pack('>IIQQQQ',END,0,sectors,0,len(encoded),0)
    return assemble([table],encoded,len(data))


def verify(binary,root):
    logical,expected=originals()
    golden=(GOLDEN/'seven-files.lzfse').read_bytes()
    checked=json.loads((GOLDEN/'modern-codecs.json').read_text())
    check(digest(golden)==checked['lzfse_sha256'] and digest(logical)==checked['logical_sha256'],'independent LZFSE input')
    raw_lzfse=b'bvx-'+struct.pack('<I',len(logical))+logical+b'bvx$'
    cases={'lzfse-compressed':(LZFSE,golden),'lzfse-stored':(LZFSE,raw_lzfse)}
    for check_id,label in [(lzma.CHECK_NONE,'none'),(lzma.CHECK_CRC32,'crc32'),(lzma.CHECK_CRC64,'crc64'),(lzma.CHECK_SHA256,'sha256')]:
        cases['xz-'+label]=(LZMA,lzma.compress(logical,format=lzma.FORMAT_XZ,check=check_id,preset=3))
    proof={}
    for label,(kind,compressed) in cases.items():
        folder=root/label;folder.mkdir()
        disk=folder/'source.dmg';data=wrapped(logical,kind,compressed);disk.write_bytes(data)
        out=folder/'media.raw';report=run(binary,'import-image','--source',disk,'--image-type','dmg','--output',out,'--json')
        check(out.read_bytes()==logical and report['sha256']==checked['logical_sha256'] and disk.read_bytes()==data,label)
        restore_files(binary,out,folder,expected)
        proof[label]=len(expected)
    for kind,stream in [(LZFSE,golden),(LZMA,cases['xz-crc32'][1])]:
        for i,wrong in enumerate([stream[:-5],stream+b'not-in-stream',stream[:1],bytes(1024)]):
            path=root/f'bad-{kind}-{i}.dmg';out=path.with_suffix('.raw')
            path.write_bytes(wrapped(logical,kind,wrong))
            run(binary,'import-image','--source',path,'--image-type','dmg','--output',out,'--json',expected=4)
            check(not out.exists(),(kind,i))
        path=root/f'length-{kind}.dmg';out=path.with_suffix('.raw')
        path.write_bytes(wrapped(logical[:-512],kind,stream))
        run(binary,'import-image','--source',path,'--image-type','dmg','--output',out,'--json',expected=4)
        check(not out.exists(),(kind,'overexpansion'))
    print(len(cases),'modern UDIF runs; each logical image and seven output files matched. Ten invalid images refused.',flush=True)
    truth=json.loads((GOLDEN/'manifest.json').read_text())
    native_dir=GOLDEN/'apple-reference'
    recorded=json.loads((native_dir/'manifest.json').read_text())['images']
    for name in ('ULFO.dmg','ULMO.dmg'):
        folder=root/name;folder.mkdir();image=native_dir/name;out=folder/'disk.raw'
        check(file_hash(image)==recorded[name]['container_sha256'],name)
        # In particular an Apple ULMO data fork starts with XZ. The outer UDIF
        # must be recognized before the standalone compressed-RAW fallback.
        result=run(binary,'import-image','--source',image,'--output',out,'--json')
        check(result['sha256']==file_hash(out)==recorded[name]['logical_sha256']==truth['disk_sha256'] and
              result['bytes']==truth['disk_bytes'],name)
        restore_files(binary,out,folder,truth['deleted'],True)
        proof[name]=len(truth['deleted'])
    print('Actual hdiutil ULFO/ULMO images: both complete disk hashes and 14 deleted file hashes matched',flush=True)
    return proof


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as t:verify(p.parse_args().binary.resolve(),Path(t))
