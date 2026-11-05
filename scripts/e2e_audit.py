"""Regression cases from the 2026-11-04 audit; fresh synthetic files only."""
import argparse
import json
import os
import struct
import subprocess
import tempfile
from pathlib import Path
from e2e import Client, check, run
from e2e_fat import scan, candidates, write_at
from make_fat_fixture import fat32, exfat, sfn, exfat_set
from make_fixture import record, filename, resident, nonresident, put, MFT_LCN, SECTOR, RECORD


def directory_order(binary, root, kind, make, reverse, descendant):
    label=f'{kind}-{reverse}-{descendant}'
    source=root/(label+'.raw');_,_,meta=make(source)
    address=lambda c:meta['heap']+(c-2)*meta['cluster']
    a,b=(350,351) if descendant else (300,301)
    if kind=='fat32':
        for c in [2,300,301,350,351]:
            for copy in range(2):write_at(source,meta['fat']+copy*meta['fat_sectors']*512+c*4,struct.pack('<I',0xfffffff))
        folder=lambda name,c:sfn(name.ljust(11).encode(),c,0,deleted=False,directory=True)
        dirs=[folder('DIRA',300),folder('DIRB',301)]
        if reverse:dirs.reverse()
        write_at(source,address(2),b''.join(dirs)+bytes(32))
        if descendant:
            write_at(source,address(300),folder('CHILDA',350)+bytes(32))
            write_at(source,address(301),folder('CHILDB',351)+bytes(32))
        write_at(source,address(a),sfn(b'PHANTOM TXT',400,17)+bytes(32))
        write_at(source,address(b),sfn(b'LIVE    BIN',300,512,deleted=False)+bytes(32))
    else:
        with source.open('rb') as f:f.seek(address(2));system=f.read(64)
        folder=lambda name,c:b''.join(exfat_set(name,c,4096,deleted=False,directory=True))
        dirs=[folder('DIRA',300),folder('DIRB',301)]
        if reverse:dirs.reverse()
        write_at(source,address(2),system+b''.join(dirs)+bytes(32))
        if descendant:
            write_at(source,address(300),folder('CHILDA',350)+bytes(32))
            write_at(source,address(301),folder('CHILDB',351)+bytes(32))
        write_at(source,address(a),b''.join(exfat_set('phantom.txt',400,17))+bytes(32))
        write_at(source,address(b),b''.join(exfat_set('live.bin',300,4096,deleted=False))+bytes(32))
        for c in [300,301,350,351]:
            at=meta['bitmap']+(c-2)//8
            with source.open('rb') as f:f.seek(at);value=f.read(1)[0]
            write_at(source,at,bytes([value|(1<<((c-2)%8))]))
    write_at(source,address(400),b'UNRELATED CONTENT')
    summary=scan(binary,source,root/(label+'-work'),expected=5)
    check(summary['invalid_records']>0,'directory ownership conflict not recorded')
    items=candidates(binary,summary['session'])
    for c in items.values():
        check(not c['supported'],'crosslinked directory provenance remained supported')
        result=run(binary,'recover','--session',summary['session'],'--id',c['id'],'--destination',root/(label+'-out'),'--json',expected=5)['results'][0]
        check(result['path'] is None,'crosslinked directory candidate wrote data')


def live_prefix(binary, root, partial):
    label='live-prefix-'+str(partial);source=root/(label+'.raw');_,_,meta=fat32(source)
    address=lambda c:meta['heap']+(c-2)*512
    for copy in range(2):
        write_at(source,meta['fat']+copy*meta['fat_sectors']*512+2*4,struct.pack('<I',0xfffffff))
        if partial:write_at(source,meta['fat']+copy*meta['fat_sectors']*512+400*4,struct.pack('<I',401))
    target=401 if partial else 400
    write_at(source,address(2),sfn(b'DELETED TXT',target,17)+sfn(b'LIVE    BIN',400,700 if partial else 17,deleted=False)+bytes(32))
    write_at(source,address(target),b'ACTIVE FILE BYTES')
    summary=scan(binary,source,root/(label+'-work'),expected=5)
    candidate=next(iter(candidates(binary,summary['session']).values()))
    check(not candidate['supported'] and candidate['allocation_status']=='conflict','known live prefix lost after FAT error')
    result=run(binary,'recover','--session',summary['session'],'--id',candidate['id'],'--destination',root/(label+'-out'),'--json',expected=5)['results'][0]
    check(result['path'] is None,'active prefix recovered as deleted')


def incomplete_exfat(binary,root,kind):
    source=root/('bad-active-'+kind+'.raw');_,_,meta=exfat(source)
    address=lambda c:meta['heap']+(c-2)*meta['cluster']
    with source.open('rb') as f:f.seek(address(2));system=f.read(64)
    live=bytearray(b''.join(exfat_set('live.bin',400,17,deleted=False)))
    if kind=='checksum':live[2]^=1
    elif kind=='count':live[1]=1
    elif kind=='critical':live=bytearray(32);live[0]=0x84
    elif kind=='directory':live=bytearray(b''.join(exfat_set('dir',401,511,deleted=False,directory=True)))
    write_at(source,address(2),system+b''.join(exfat_set('deleted.txt',400,17))+live+bytes(32))
    summary=scan(binary,source,root/('bad-active-'+kind+'-work'),expected=5)
    candidate=next(iter(candidates(binary,summary['session']).values()))
    check(candidate['allocation_status']=='unknown' and any('incomplete' in w for w in candidate['warnings']), 'incomplete live ownership presented as known free')


def output_swap(binary,root):
    source=root/'swap.raw';_,_,meta=fat32(source)
    count=10000;payload=b'AUDIT RECOVERY DATA';fat=bytearray(meta['fat_sectors']*512)
    struct.pack_into('<II',fat,0,0xffffff8,0xfffffff)
    for c in range(2,2+count):struct.pack_into('<I',fat,c*4,c+1 if c<1+count else 0xfffffff)
    slots=[sfn(b'DELETED TXT',50000,len(payload))]
    slots.extend(sfn(f'F{i:07d}TXT'.encode(),0,0,deleted=False) for i in range(count*16-2))
    with source.open('r+b') as f:
        for copy in range(2):f.seek(meta['fat']+copy*meta['fat_sectors']*512);f.write(fat)
        f.seek(meta['heap']);f.write(b''.join(slots)+bytes(32))
        f.seek(meta['heap']+(50000-2)*512);f.write(payload)
    allowed=root/'allowed';target=allowed/'target';target.mkdir(parents=True)
    outside=root/'outside';outside.mkdir()
    client=Client(binary,source,root/'mcp-work',allowed)
    try:
        client.rpc('initialize',{'protocolVersion':'2025-11-25'})
        scanned=client.wait(client.tool('start_scan',{'source':str(source),'request_id':'scan'})['job_id'])
        check(scanned['state']=='completed',scanned)
        sid=scanned['result']['session_id'];items=client.tool('list_candidates',{'session_id':sid})['candidates']
        job=client.tool('recover_files',{'session_id':sid,'ids':[items[0]['id']],'destination':str(target),'request_id':'recover'})
        swapped=False
        try:
            target.rename(allowed/'original-target')
            if os.name=='nt':
                subprocess.run(['cmd','/c','mklink','/J',str(target),str(outside)],check=True,capture_output=True)
            else:target.symlink_to(outside,target_is_directory=True)
            swapped=True
        except PermissionError:
            # Windows may prevent renaming the pinned directory handle.
            check(os.name=='nt','unexpected rename failure')
        recovered=client.wait(job['job_id'])
        check(not list(outside.iterdir()),'MCP wrote a file or report outside the allowed root')
        if swapped:check(recovered['state']=='failed' and recovered['result']['error']['code']=='DESTINATION_CHANGED',recovered)
        else:check(recovered['state']=='completed',recovered)
    finally:client.close()


def large_session(binary,root):
    source=root/'large-session.raw';records=8000;size=9*1024*1024
    boot=bytearray(512);boot[:3]=b'\xeb\x52\x90';boot[3:11]=b'NTFS    ';put(boot,11,'H',512)
    boot[13]=1;put(boot,40,'Q',size//512-1);put(boot,48,'Q',4);boot[64]=246;boot[510:]=b'\x55\xaa'
    with source.open('xb') as f:
        f.truncate(size);f.seek(0);f.write(boot);f.seek(size-512);f.write(boot)
        def wr(n,attrs,flags=0,sequence=7):
            f.seek(MFT_LCN*SECTOR+n*RECORD);f.write(record(n,attrs,flags,sequence))
        wr(0,[nonresident([(records*2,MFT_LCN)],records*RECORD)],1,1)
        wr(5,[filename('.')],3,1)
        for n in range(10,110):wr(n,[filename(f'D{n:03d}'+'x'*196,parent=5 if n==10 else n-1,parent_sequence=1)],3,1)
        for n in range(200,7200):wr(n,[filename(f'file-{n}.txt',parent=109,parent_sequence=1),resident(0x80,b'x')])
    p=subprocess.run([str(binary),'scan','--source',str(source),'--work-dir',str(root/'large-work'),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=90)
    check(p.returncode==5,p.stderr)
    summary=json.loads(p.stdout);session=Path(summary['session'])
    check(summary['truncated'] and 0<summary['candidates']<7000,summary)
    check(session.stat().st_size<=128*1024*1024,'saved an unloadable session')
    item=run(binary,'results','--session',session,'--limit','1','--json')['candidates'][0]
    result=run(binary,'recover','--session',session,'--id',item['id'],'--destination',root/'large-out','--json',timeout=90)['results'][0]
    check(result['status']=='completed' and Path(result['path']).read_bytes()==b'x','bounded session cannot be restored')


def main(binary):
    with tempfile.TemporaryDirectory(prefix='rescue-audit-regression-') as temporary:
        root=Path(temporary)
        for kind,make in [('fat32',fat32),('exfat',exfat)]:
            for reverse in [False,True]:
                for descendant in [False,True]:directory_order(binary,root,kind,make,reverse,descendant)
        for partial in [False,True]:live_prefix(binary,root,partial)
        for kind in ['checksum','count','critical','directory']:incomplete_exfat(binary,root,kind)
        output_swap(binary,root)
        large_session(binary,root)
    print('Audit regressions: 16 cases passed (FAT ancestry/prefix, exFAT uncertainty, MCP swap, 7000-file session)')


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True)
    main(parser.parse_args().binary.resolve())
