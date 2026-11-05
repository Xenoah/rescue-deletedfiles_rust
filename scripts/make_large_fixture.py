"""Chunked sparse NTFS oracle; never allocates the volume or MFT in memory.

The 1 TiB size describes a sparse logical image, not a physical 1 TiB read test.
All deleted files contain deterministic resident data; this measures enumeration,
indexing and selection, not every filesystem or recovery scenario.
"""
import argparse, hashlib, json, struct
from pathlib import Path
from make_fixture import record, filename, resident, nonresident, SECTOR, RECORD, MFT_LCN


def build(path, count, logical_bytes=None, late_record=None):
    if not 1 <= count <= 10_000_000:
        raise ValueError('count must be 1..10M')
    records=max(count+16, (late_record+1) if late_record is not None else 0)
    minimum=((MFT_LCN*SECTOR+records*RECORD+4095)//4096+1)*4096
    size=logical_bytes or minimum
    if size < minimum or size % SECTOR:
        raise ValueError('logical size must cover the MFT and backup boot')
    boot=bytearray(SECTOR);boot[:3]=b'\xeb\x52\x90';boot[3:11]=b'NTFS    '
    struct.pack_into('<H',boot,11,SECTOR);boot[13]=1
    struct.pack_into('<Q',boot,40,size//SECTOR-1)
    struct.pack_into('<Q',boot,48,MFT_LCN);boot[64]=246
    struct.pack_into('<Q',boot,72,0x1020304050607080);boot[510:512]=b'\x55\xaa'
    payload=b'Rescue deterministic large-index byte oracle.\n'
    template=record(16,[filename('file-00000000.bin'),resident(0x80,payload)])
    name_at=template.index('00000000'.encode('utf-16le'))
    examples={}
    with Path(path).open('xb') as f:
        f.truncate(size);f.write(boot);f.seek(size-SECTOR);f.write(boot)
        f.seek(MFT_LCN*SECTOR);f.write(record(0,[nonresident([(records*2,MFT_LCN)],records*RECORD)],flags=1,sequence=1))
        f.seek(MFT_LCN*SECTOR+5*RECORD);f.write(record(5,[filename('.')],flags=3,sequence=1))
        f.seek(MFT_LCN*SECTOR+16*RECORD)
        for start in range(0,count,4096):
            n=min(4096,count-start);chunk=bytearray(template*n)
            for j in range(n):
                number=start+j;at=j*RECORD
                struct.pack_into('<I',chunk,at+44,number+16)
                chunk[at+name_at:at+name_at+16]=f'{number:08d}'.encode('utf-16le')
            f.write(chunk)
        for number in sorted({0,count//2,count-1}):
            examples[f'mft-{number+16}-7']={'name':f'file-{number:08d}.bin','hex':payload.hex(),'sha256':hashlib.sha256(payload).hexdigest()}
        if late_record is not None:
            f.seek(MFT_LCN*SECTOR+late_record*RECORD)
            f.write(record(late_record,[filename('late-oracle.txt'),resident(0x80,payload)]))
            examples[f'mft-{late_record}-7']={'name':'late-oracle.txt','hex':payload.hex(),'sha256':hashlib.sha256(payload).hexdigest()}
    return {'logical_bytes':size,'mft_records':records,'candidates':count+(late_record is not None),'examples':examples,
            'scope':'Synthetic sparse NTFS image; resident payloads; no physical disk throughput claim.'}


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('path',type=Path);p.add_argument('--count',type=int,required=True);p.add_argument('--logical-bytes',type=int);p.add_argument('--late-record',type=int);a=p.parse_args()
    result=build(a.path,a.count,a.logical_bytes,a.late_record)
    a.path.with_suffix('.truth.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8');print(json.dumps(result))
