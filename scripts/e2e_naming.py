"""Revalidated original names and explicit transformations in real CLI/MCP reports."""
import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import zipfile
import jsonschema
from e2e import Client, check, run
from e2e_disks import disk_image
from make_fixture import build, record, filename, resident, MFT_LCN, SECTOR, RECORD
from make_ui_fixture import build as build_ui

ROOT=Path(__file__).resolve().parent.parent


def main(binary, output):
    schema=json.loads((ROOT/'schemas/v1.json').read_text())
    validator=jsonschema.Draft202012Validator({'$defs':schema['$defs'],'$ref':'#/$defs/RecoveryResult'})
    cases=[];exact=0
    def verify(result, expected):
        nonlocal exact
        validator.validate(result)
        check(result['status']=='completed' and Path(result['path']).read_bytes()==expected,result)
        check(result['sha256']==hashlib.sha256(expected).hexdigest(),result)
        exact+=1
        return result['naming']
    def changes(naming, reason):return [c for c in naming['changes'] if c['reason']==reason]
    with tempfile.TemporaryDirectory(prefix='rescue-naming-') as temporary:
        root=Path(temporary);source=root/'source.raw';build_ui(source);before=hashlib.sha256(source.read_bytes()).hexdigest()
        pdf=(ROOT/'tests/corpus/files/sample.pdf').read_bytes()
        scanned=run(binary,'scan','--source',source,'--work-dir',root/'work','--json');session=Path(scanned['session'])
        saved=json.loads(session.read_text(encoding='utf-8'));target=next(c for c in saved['candidates'] if c['name']=='sample.pdf')
        dest=root/'out';(dest/'01_書類').mkdir(parents=True);(dest/'01_書類/sample.pdf').write_bytes(b'keep existing')
        first=run(binary,'recover','--session',session,'--id',target['id'],'--destination',dest,'--json')['results'][0]
        naming=verify(first,pdf)
        check(naming['basis']=='metadata_verified_path' and naming['original_name']=='sample.pdf' and naming['original_path']=='01_書類/sample.pdf',naming)
        change=changes(naming,'file_collision');check(len(change)==1 and Path(change[0]['from']).name=='sample.pdf' and Path(change[0]['to']).name=='sample (復元 1).pdf',change)
        check((dest/'01_書類/sample.pdf').read_bytes()==b'keep existing','clobbered original')
        cases.append('original_metadata_and_file_collision_history')
        blocked=root/'blocked';blocked.mkdir();(blocked/'01_書類').write_bytes(b'keep folder blocker')
        got=run(binary,'recover','--session',session,'--id',target['id'],'--destination',blocked,'--json')['results'][0]
        naming=verify(got,pdf);change=changes(naming,'directory_collision')
        check(len(change)==1 and Path(change[0]['from']).name=='01_書類' and Path(change[0]['to']).name=='01_書類 (復元 1)',change)
        check((blocked/'01_書類').read_bytes()==b'keep folder blocker','clobbered blocker');cases.append('directory_collision_history')
        edited=json.loads(json.dumps(saved));forged=next(c for c in edited['candidates'] if c['id']==target['id']);forged.update(name='forged.exe',original_path='../forged.exe',path_verified=True)
        tampered=root/'tampered.json';tampered.write_text(json.dumps(edited),encoding='utf-8')
        for stream in [False,True]:
            args=['--stream'] if stream else []
            result=run(binary,'recover','--session',tampered,'--id',target['id'],'--destination',root/('stream' if stream else 'normal'),*args,'--json')['results'][0]
            naming=verify(result,pdf);check(naming['original_name']=='sample.pdf' and naming['original_path']=='01_書類/sample.pdf' and 'forged' not in json.dumps(naming),naming)
            if stream:
                check(changes(naming,'streaming_flat_output') and changes(naming,'unique_output_name'),naming)
                check(changes(naming,'unique_output_name')[-1]['to']==Path(result['path']).name,naming)
            cases.append(('stream' if stream else 'batch')+'_ignores_forged_saved_names')
        skipped=run(binary,'recover','--session',session,'--id',target['id'],'--destination',dest,'--collision','skip','--json')['results'][0]
        validator.validate(skipped);check(skipped['status']=='skipped' and skipped['naming']['original_name']=='sample.pdf' and not changes(skipped['naming'],'file_collision'),skipped)
        cases.append('skipped_file_has_no_invented_rename')
        forged['record_sha256']='0'*64;tampered.write_text(json.dumps(edited),encoding='utf-8')
        invalid=run(binary,'recover','--session',tampered,'--id',target['id'],'--destination',root/'invalid','--json',expected=5)['results'][0]
        validator.validate(invalid);check(invalid['path'] is None and 'naming' not in invalid,'unvalidated source name asserted')
        cases.append('failed_metadata_has_no_trusted_name')
        disk=root/'disk.raw';disk_image(disk,[source],512,'mbr')
        full=run(binary,'scan','--source',disk,'--work-dir',root/'disk-work','--json')
        prefixed=run(binary,'recover','--session',full['session'],'--id','p1:'+target['id'],'--destination',root/'partition','--json')['results'][0]
        naming=verify(prefixed,pdf);check(changes(naming,'partition_namespace')==[{'reason':'partition_namespace','from':'01_書類/sample.pdf','to':'P1/01_書類/sample.pdf'}],naming)
        cases.append('whole_disk_output_namespace')
        names=root/'names.raw';build(names)
        with names.open('r+b') as f:
            for number in [23,24,25]:
                f.seek(MFT_LCN*SECTOR+number*RECORD);f.write(record(number,[filename('rescue-report.json'),resident(0x80,f'payload-{number}'.encode())]))
            f.seek(MFT_LCN*SECTOR+26*RECORD);f.write(record(26,[filename('CON.txt'),resident(0x80,b'reserved device name')]))
            f.seek(MFT_LCN*SECTOR+27*RECORD);f.write(record(27,[filename('base.txt'),resident(0x80,b'base'),resident(0x80,b'ADS bytes',name='Zone.Identifier')]))
            f.seek(MFT_LCN*SECTOR+28*RECORD);f.write(record(28,[filename('bad:name?.txt'),resident(0x80,b'portable name')]))
        names_scan=run(binary,'scan','--source',names,'--work-dir',root/'names-work','--json')
        candidates=run(binary,'results','--session',names_scan['session'],'--limit',100,'--json')['candidates']
        zipped=run(binary,'recover','--session',names_scan['session'],'--id','mft-23-7','--id','mft-24-7','--id','mft-25-7','--archive',root/'names.zip','--json')['result']
        with zipfile.ZipFile(zipped['archive']) as archive:
            check(archive.testzip() is None and len(set(n.lower() for n in archive.namelist()))==4,'duplicate archive entry')
            report=json.loads(archive.read('rescue-report.json'));check(report['results']==zipped['results'],'ZIP report differs')
            for number,result in enumerate(zipped['results'],23):
                validator.validate(result);expected_name=f'rescue-report (復元 {number-22}).json'
                check(result['archive_entry']==expected_name and archive.read(expected_name)==f'payload-{number}'.encode(),result)
                check(changes(result['naming'],'archive_collision')==[{'reason':'archive_collision','from':'rescue-report.json','to':expected_name}],result)
                exact+=1
        cases.append('archive_report_and_payload_name_collisions')
        for name,payload,expected in [('CON.txt',b'reserved device name','_CON.txt'),('bad:name?.txt',b'portable name','bad_name_.txt')]:
            target_name=next(c for c in candidates if c['name']==name)
            result=run(binary,'recover','--session',names_scan['session'],'--id',target_name['id'],'--destination',root/'portable','--json')['results'][0]
            naming=verify(result,payload);check(Path(result['path']).name==expected and naming['original_name']==name and changes(naming,'portable_name'),naming)
            cases.append('portable_'+name)
        stream=next(c for c in candidates if c['stream_name']=='Zone.Identifier')
        result=run(binary,'recover','--session',names_scan['session'],'--id',stream['id'],'--destination',root/'ads','--json')['results'][0]
        naming=verify(result,b'ADS bytes');check(naming['stream_name']=='Zone.Identifier' and changes(naming,'alternate_stream') and ':' not in Path(result['path']).name,naming)
        cases.append('alternate_stream_keeps_source_name')
        unknown=root/'unknown.raw';build(unknown)
        result=run(binary,'restore','--source',unknown,'--name','stale-parent.txt','--destination',root/'unknown-out','--json')['results'][0]
        validator.validate(result);check(result['status']=='completed' and result['naming']['basis']=='metadata_unverified_path',result)
        cases.append('unverified_hierarchy_remains_explicit')
        png=(ROOT/'tests/corpus/files/sample.png').read_bytes();raw=root/'carved.raw';raw.write_bytes(bytes(512)+png+bytes(512))
        raw_scan=run(binary,'scan','--source',raw,'--mode','carve','--extension','png','--work-dir',root/'raw-work','--json')
        result=run(binary,'recover','--session',raw_scan['session'],'--all','--destination',root/'raw-out','--json')['results'][0]
        naming=verify(result,png);check(naming['basis']=='generated' and naming['original_name'] is None and naming['original_path'] is None,naming)
        cases.append('generated_name_is_not_an_original_name')
        mcp_root=root/'mcp-out';(mcp_root/'01_書類').mkdir(parents=True);(mcp_root/'01_書類/sample.pdf').write_bytes(b'keep')
        with closing(Client(binary,source,root/'mcp-work',mcp_root)) as client:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'});job=client.wait(client.tool('start_scan',{'source':str(source),'request_id':'scan'})['job_id'])
            result=client.wait(client.tool('recover_files',{'session_id':job['result']['session_id'],'ids':[target['id']],'destination':str(mcp_root),'request_id':'restore'})['job_id'])['result']['results'][0]
            naming=verify(result,pdf);check(changes(naming,'file_collision') and naming['original_name']=='sample.pdf',naming)
        cases.append('mcp_name_contract_and_actual_collision')
        check(hashlib.sha256(source.read_bytes()).hexdigest()==before,'source changed')
    proof={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':len(cases),'cases':cases,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:
        output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(proof,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(proof))


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output.resolve() if a.output else None)
