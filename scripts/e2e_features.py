"""Advanced search, filesystem dates and real ZIP recovery through CLI and MCP.

Independent Python zipfile/CRC and checked-in format bytes serve as oracles.
All modifications target fresh, disposable regular images.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import struct
import tempfile
import zipfile
from e2e import Client, check, run
from e2e_formats import corpus_load, ntfs_image
from make_fixture import build, record, filename, resident, put, MFT_LCN, SECTOR, RECORD
from make_carve_fixture import png
from make_fat_fixture import fat32, exfat, rotate_sum


def scan(binary, source, work, *flags, expected=0):
    result=run(binary,'scan','--source',source,'--work-dir',work,'--json',*flags,expected=expected)
    return Path(result['session']), run(binary,'results','--session',result['session'],'--limit','1000','--json')['candidates']


def verify_zip(path, outcome, truth):
    check(outcome['status']=='completed' and Path(outcome['archive']).samefile(path), outcome)
    check(hashlib.sha256(path.read_bytes()).hexdigest()==outcome['sha256'],'ZIP SHA mismatch')
    with zipfile.ZipFile(path) as archive:
        check(archive.testzip() is None,'ZIP CRC failure')
        report=json.loads(archive.read('rescue-report.json'))
        check(report['results']==outcome['results'],'embedded and external results differ')
        check(set(r['candidate_id'] for r in report['results'])==set(truth),'missing ZIP selection')
        check(len(archive.namelist())==len(truth)+1,'unexpected ZIP entry')
        for result in report['results']:
            entry=result['archive_entry']
            check(not entry.startswith(('/', '\\')) and '..' not in Path(entry).parts and '\\' not in entry and ':' not in entry,'archive path traversal')
            data=archive.read(entry)
            expected=truth[result['candidate_id']]['data']
            check(data==expected and hashlib.sha256(data).hexdigest()==result['sha256'],'ZIP bytes differ')
            check(Path(result['path']).samefile(path),'stale staging path exposed')


def main(binary):
    checks=0
    with tempfile.TemporaryDirectory(prefix='rescue-features-') as temp:
        root=Path(temp); source=root/'dates.raw'; truth=build(source)
        # Distinct SI and FILE_NAME dates prove that SI takes precedence.
        created=int(datetime(2024,2,29,0,0,tzinfo=timezone.utc).timestamp())
        si=bytearray(72);put(si,0,'Q',(created+11644473600)*10_000_000);put(si,8,'Q',(created+3600+11644473600)*10_000_000)
        fn=filename('日本語.txt',10,2);put(fn,24+8,'Q',11644473600*10_000_000)
        content=record(20,[resident(0x10,si),fn,resident(0x80,bytes.fromhex(truth['mft-20-7']['hex']))])
        with source.open('r+b') as f:f.seek(MFT_LCN*SECTOR+20*RECORD);f.write(content)
        session,candidates=scan(binary,source,root/'dates-work')
        item=next(c for c in candidates if c['id']=='mft-20-7')
        check(item['created']=={'value':'2024-02-29T00:00:00Z','unix_seconds':created},item)
        check(item['modified']['unix_seconds']==created+3600,item)
        checks+=2
        _,items=scan(binary,source,root/'scope','--path-prefix','写真','--max-depth','0','--extension','.TXT')
        check([c['id'] for c in items]==['mft-20-7'],'folder/depth/extension scope failed');checks+=1
        _,items=scan(binary,source,root/'root','--max-depth','0','--extension','txt')
        check('mft-20-7' not in [c['id'] for c in items] and 'mft-23-7' in [c['id'] for c in items],'depth zero not direct children');checks+=1
        _,items=scan(binary,source,root/'prefix','--path-prefix','写')
        check(not items,'path prefix must end on folder boundary');checks+=1
        run(binary,'scan','--source',source,'--work-dir',root/'invalid','--extension','.','--json',expected=2)
        run(binary,'scan','--source',source,'--work-dir',root/'invalid','--range-start','1','--json',expected=2)
        run(binary,'scan','--source',source,'--work-dir',root/'invalid','--max-depth','257','--json',expected=2);checks+=3
        # Carving range boundaries are strict: incomplete files stay unsupported.
        raw=root/'range.raw';data=png();raw.write_bytes(b'\0'*123+data+b'\0'*100+data)
        _,items=scan(binary,raw,root/'range-work','--mode','carve','--range-start','123','--range-end',str(123+len(data)))
        check(len(items)==1 and items[0]['supported'] and items[0]['record']==123,'range/absolute offset');checks+=1
        _,items=scan(binary,raw,root/'short-work','--mode','carve','--range-start','123','--range-end',str(122+len(data)),expected=5)
        check(len(items)==1 and not items[0]['supported'],'carving crossed selected end');checks+=1
        run(binary,'scan','--source',raw,'--work-dir',root/'invalid-range','--mode','carve','--range-end',str(raw.stat().st_size+1),'--json',expected=2);checks+=1
        # Both FAT date encodings, including exFAT's explicit UTC+9 zone.
        date=((2024-1980)<<9)|(2<<5)|29;clock=(9<<11)|(13<<5)|7
        for kind,builder in [('fat32',fat32),('exfat',exfat)]:
            image=root/(kind+'.raw');_,_,meta=builder(image)
            with image.open('r+b') as f:
                f.seek(meta['heap']); directory=bytearray(f.read(meta['cluster']))
                if kind=='fat32':
                    at=next(i for i in range(0,len(directory),32) if directory[i]==0xe5 and directory[i+11]!=15 and not directory[i+11]&16)
                    put(directory,at+14,'H',clock);put(directory,at+16,'H',date);put(directory,at+22,'H',clock);put(directory,at+24,'H',date)
                else:
                    at=next(i for i in range(0,len(directory),32) if directory[i]==5)
                    put(directory,at+8,'I',(date<<16)|clock);put(directory,at+12,'I',(date<<16)|clock);directory[at+22]=directory[at+23]=0x80|36
                    count=directory[at+1]+1;restored=bytearray(directory[at:at+count*32])
                    for i in range(0,len(restored),32):restored[i]|=0x80
                    put(directory,at+2,'H',rotate_sum(restored[:2]+restored[4:],16))
                f.seek(meta['heap']);f.write(directory)
            _,items=scan(binary,image,root/(kind+'-work'))
            dated=[c for c in items if c.get('created')]
            check(dated and dated[0]['created']['value'].startswith('2024-02-29T09:13:14'),dated)
            check((dated[0]['created']['unix_seconds'] is None)==(kind=='fat32'),'invented or missing timezone');checks+=2
        # Every one of 35 format families goes into a real ZIP, byte-for-byte.
        corpus=list(corpus_load(Path(__file__).resolve().parent.parent/'tests/corpus'))
        all_source=root/'formats.raw';all_truth,_=ntfs_image(all_source,corpus,'fragmented')
        all_session,_=scan(binary,all_source,root/'format-work')
        archive=root/'recovered.zip'
        result=run(binary,'recover','--session',all_session,'--all','--archive',archive,'--work-dir',root/'stage','--json')['result']
        verify_zip(archive,result,all_truth);checks+=len(all_truth)+2
        before=archive.read_bytes()
        duplicate=run(binary,'recover','--session',all_session,'--all','--archive',archive,'--json')['result']
        check(duplicate['status']=='completed' and Path(duplicate['archive'])!=archive,'duplicate ZIP was not renamed')
        check(archive.read_bytes()==before,'existing ZIP overwritten');checks+=1
        # An old session without the optional new date/search fields still works.
        legacy=json.loads(session.read_text(encoding='utf-8'));legacy.pop('search',None)
        for c in legacy['candidates']:c.pop('created',None);c.pop('modified',None)
        old=root/'legacy.json';old.write_text(json.dumps(legacy),encoding='utf-8')
        run(binary,'recover','--session',old,'--id','mft-20-7','--destination',root/'legacy-out','--json');checks+=1
        client=Client(binary,all_source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            job=client.wait(client.tool('start_scan',{'source':str(all_source),'extensions':['png'],'path_prefix':'全形式','max_depth':0,'request_id':'filtered'})['job_id'])
            check(job['state']=='completed' and job['result']['candidates']==2,job)
            sid=job['result']['session_id'];items=client.tool('list_candidates',{'session_id':sid})['candidates'];chosen={c['id']:all_truth[c['id']] for c in items}
            client.tool('recover_files',{'session_id':sid,'ids':list(chosen),'destination':str(root/'outside.zip'),'output_format':'zip','request_id':'denied'},error=True)
            task=client.tool('recover_files',{'session_id':sid,'ids':list(chosen),'destination':str(root/'mcp-out'/'mcp.zip'),'output_format':'zip','request_id':'zip'})
            result=client.wait(task['job_id']);check(result['state']=='completed',result)
            verify_zip(root/'mcp-out'/'mcp.zip',result['result']['archive'],chosen);checks+=5
        finally:client.close()
    print(json.dumps({'status':'passed','checks':checks,'scope':'search/date/ZIP/legacy/CLI/MCP, 35 formats'}))

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True)
    main(p.parse_args().binary.resolve())
