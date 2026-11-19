"""Check Apple UDIF logical sectors, integrity and real FAT32 file deletions."""
import argparse
import bz2
import hashlib
import json
from pathlib import Path
import plistlib
import struct
import subprocess
import tempfile
import zlib

from e2e import check, run

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / 'tests' / 'corpus' / 'files'
GOLDEN = ROOT / 'tests' / 'udif'
ADC, ZLIB, BZIP = 0x80000004, 0x80000005, 0x80000006
END = 0xffffffff


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_hash(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def crc_header(data):
    return struct.pack('>III', 2, 32, zlib.crc32(data))


def adc_encode(data):
    # A short and a long backwards copy exercise the Apple ADC overlap rule.
    out = bytearray()
    i = 0
    while i < len(data):
        if i and data[i:i+67] == data[i-1:i]*67:
            out.extend((0x7f, 0, 0))
            i += 67
        elif i and data[i:i+3] == data[i-1:i]*3:
            out.extend((0, 0))
            i += 3
        else:
            literal = data[i:i+128]
            out.append(127 + len(literal))
            out.extend(literal)
            i += len(literal)
    return bytes(out)


def assemble(parts, data_fork, disk_bytes, prefix=b''):
    checksums = b''.join(p[72:76] for p in parts)
    xml = plistlib.dumps({'resource-fork':{'blkx':[{'Data':bytes(p),'Name':str(i),'ID':str(i)}
                                                  for i,p in enumerate(parts)]}})
    footer = bytearray(512)
    struct.pack_into('>4sIIIQQQQQII',footer,0,b'koly',4,512,1,0,len(prefix),len(data_fork),0,0,1,1)
    footer[80:92] = crc_header(data_fork)
    struct.pack_into('>QQ',footer,216,len(prefix)+len(data_fork),len(xml))
    footer[352:364] = crc_header(checksums)
    struct.pack_into('>IQ',footer,488,1,disk_bytes//512)
    return prefix + data_fork + xml + footer


def fixture(logical, kind, prefix=b''):
    parts, fork = [], bytearray()
    for start in (0, len(logical)//2):
        data = logical[start:start+len(logical)//2]
        table = bytearray(204)
        base = len(fork) if prefix else 0
        struct.pack_into('>4sIQQQ',table,0,b'mish',1,start//512,len(data)//512,base)
        table[64:76] = crc_header(data)
        rows = []
        packed_part = bytearray()
        for pos in range(0,len(data),4096):
            chunk = data[pos:pos+4096]
            codec = [1, ZLIB, BZIP, ADC][pos//4096 % 4] if kind=='mixed' else kind
            if kind=='mixed' and not any(chunk):
                codec = 0 if pos//4096 % 2 else 2
            encode = {0:lambda _:b'',2:lambda _:b'',1:bytes,ZLIB:zlib.compress,BZIP:bz2.compress,ADC:adc_encode}
            packed = encode[codec](chunk)
            rows.append(struct.pack('>IIQQQQ',codec,0,pos//512,len(chunk)//512,len(fork)-base+len(packed_part),len(packed)))
            packed_part.extend(packed)
        rows.append(struct.pack('>IIQQQQ',END,0,len(data)//512,0,len(fork)-base+len(packed_part),0))
        struct.pack_into('>I',table,200,len(rows))
        parts.append(table+b''.join(rows))
        fork.extend(packed_part)
    # The prefixed case also puts the block-map dictionaries in reverse order.
    return assemble(parts[::-1] if prefix else parts,fork,len(logical),prefix),parts,fork


def restore_files(binary, image, root, expected, metadata=False):
    args=[] if metadata else ['--mode','carve']
    scan=run(binary,'scan','--source',image,'--work-dir',root/'sessions','--json',*args)
    rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
    ids=[r['id'] for r in rows if r['supported'] and (metadata or r.get('record_sha256') in expected.values())]
    check(len(ids)==len(expected),rows)
    args=[v for id_ in ids for v in ('--id',id_)]
    results=run(binary,'recover','--session',scan['session'],'--destination',root/'files','--json',*args)['results']
    checks={Path(r['path']).name if metadata else r['candidate_id']:file_hash(Path(r['path'])) for r in results}
    check(sorted(checks.values())==sorted(expected.values()) and all(r['sha256'] in expected.values() for r in results),checks)
    if metadata:check(checks==expected,checks)


def verify(binary, root, independent=False):
    names=['sample.'+ext for ext in ('rtf','xml','mp3','flac','tif','png','webp')]
    data={name:(CORPUS/name).read_bytes() for name in names}
    expected={k:digest(v) for k,v in data.items()}
    pieces=[b'\xe9'*512]
    for value in data.values(): pieces.extend((value,bytes(383)))
    payload=b''.join(pieces)
    logical=payload.ljust(((len(payload)+32767)//32768+1)*32768,b'\0')
    proof={}
    for label,kind,prefix in [('raw',1,b''),('zlib',ZLIB,b''),('bzip2',BZIP,b''),('adc',ADC,b''),
                              ('mixed','mixed',b''),('data-fork-offset','mixed',bytes(2048))]:
        case=root/label;case.mkdir()
        image=case/'container.dmg';raw=case/'converted.raw'
        encoded,parts,fork=fixture(logical,kind,prefix)
        image.write_bytes(encoded)
        flags=['--image-type','dmg'] if label in ('raw','adc','mixed') else []
        report=run(binary,'import-image','--source',image,'--output',raw,'--json',*flags)
        check(raw.read_bytes()==logical and report['sha256']==digest(logical) and report['bytes']==len(logical),label)
        restore_files(binary,raw,case,expected)
        if independent and not prefix:
            other=case/'dmg2img.raw'
            subprocess.run(['dmg2img','-s','-i',str(image),'-o',str(other)],check=True,capture_output=True)
            check(other.read_bytes()==logical,'dmg2img '+label)
            if label=='zlib':
                qemu=case/'qemu.raw'
                subprocess.run(['qemu-img','convert','-f','dmg','-O','raw',image,qemu],check=True)
                check(qemu.read_bytes()==logical,'QEMU zlib')
        check(image.read_bytes()==encoded,label+' source')
        proof[label]=len(expected)
        print(label,len(expected),'file hashes and full logical image matched',flush=True)
    reference=GOLDEN/'apm-fat32.dmg'
    truth=json.loads((GOLDEN/'manifest.json').read_text())
    check(file_hash(reference)==truth['container_sha256'],'external fixture')
    case=root/'libdmg';case.mkdir()
    raw=case/'disk.raw'
    report=run(binary,'import-image','--source',reference,'--output',raw,'--json')
    check(file_hash(raw)==report['sha256']==truth['disk_sha256'] and report['bytes']==truth['disk_bytes'],'libdmg entire disk')
    restore_files(binary,raw,case,truth['deleted'],True)
    proof['libdmg-apm-fat32']=len(truth['deleted'])
    print('libdmg',len(truth['deleted']),'actual FAT32 file deletions byte-exact',flush=True)

    valid,parts,fork=fixture(logical,ZLIB)
    def table_edit(off,replacement):
        table=bytearray(parts[0]);table[off:off+len(replacement)]=replacement
        return assemble([table,parts[1]],fork,len(logical))
    def tail_edit(off,replacement):
        v=bytearray(valid);idx=len(v)-512+off;v[idx:idx+len(replacement)]=replacement
        return v
    def damaged_stream():
        v=bytearray(fork);v[4]^=128
        return assemble(parts,v,len(logical))
    bad=[valid[:-10], tail_edit(4,struct.pack('>I',3)),tail_edit(60,struct.pack('>I',2)),
         tail_edit(492,bytes(8)),tail_edit(224,struct.pack('>Q',20<<20)),
         tail_edit(216,bytes(8)),tail_edit(88,b'BAD!'),tail_edit(360,b'BAD!'),
         table_edit(72,b'CRC!'),table_edit(200,b'\xff'*4),table_edit(204,struct.pack('>I',0x80007777)),
         table_edit(212,struct.pack('>Q',1)),table_edit(228,b'\xff'*8),
         table_edit(204+32,struct.pack('>Q',0)),table_edit(8,struct.pack('>Q',1)),
         table_edit(len(parts[0])-40,struct.pack('>I',0x7ffffffe)),damaged_stream()]
    for n,encoded in enumerate(bad):
        image=root/f'bad-{n}.dmg';image.write_bytes(encoded)
        out=root/f'bad-{n}.raw'
        run(binary,'import-image','--source',image,'--image-type','udif','--output',out,'--json',expected=4)
        check(not out.exists() and image.read_bytes()==encoded,n)
    print(len(bad),'damaged, incomplete or unsupported images refused',flush=True)
    return proof

def apple_images(binary,root):
    """Generate Apple UDIF images with the system DiskImages framework."""
    gold=GOLDEN/'apm-fat32.dmg'
    truth=json.loads((GOLDEN/'manifest.json').read_text())
    # Give hdiutil the same known logical media for every encoder.
    original=root/'apple-original.raw'
    run(binary,'import-image','--source',gold,'--output',original,'--json')
    check(file_hash(original)==truth['disk_sha256'],'Apple input disk')
    for kind in ('UDRO','UDZO','UDBZ','UDCO'):
        image=root/(kind+'.dmg')
        subprocess.run(['hdiutil','convert',str(original),'-imagekey','diskimage-class=CRawDiskImage',
                        '-format',kind,'-o',str(image)],check=True)
        out=root/(kind+'.raw')
        report=run(binary,'import-image','--source',image,'--image-type','dmg','--output',out,'--json')
        check(file_hash(out)==report['sha256']==truth['disk_sha256'],kind)
        restore_files(binary,out,root/kind,truth['deleted'],True)
        print('Apple hdiutil',kind,'whole disk and seven deleted files matched',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--independent',action='store_true')
    parser.add_argument('--hdiutil',action='store_true',help='Test four actual Apple DiskImages encoders on macOS')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        if args.hdiutil:
            apple_images(args.binary.resolve(),Path(temp))
        else:
            verify(args.binary.resolve(),Path(temp),args.independent)
