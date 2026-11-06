"""Real source object identities, scoped candidates and backward-compatible sessions."""
import argparse, hashlib, json, os, shutil, subprocess, tempfile, zipfile
from contextlib import closing
from pathlib import Path
import jsonschema
from e2e import Client, check, run, candidate_evidence
from make_fixture import build
from e2e_disks import disk_image


def main(binary, output):
    schema=json.loads((Path(__file__).resolve().parent.parent/'schemas/v1.json').read_text())
    checks=[];exact=0;validated=0
    def valid(name,value):
        nonlocal validated
        jsonschema.Draft202012Validator({'$defs':schema['$defs'],'$ref':'#/$defs/'+name},format_checker=jsonschema.FormatChecker()).validate(value)
        validated+=1
    def inspect(path):
        layout=run(binary,'inspect','--source',path,'--json')['layout'];valid('Source',layout)
        info=layout['source_info'];valid('SourceInfo',info)
        check(info['bytes']==layout['bytes'] and info['length']==layout['bytes'] and info['offset']==0,'source range')
        return info
    def rows(session,sid):
        page=run(binary,'results','--session',session,'--limit',1000,'--json')
        check(page['session_id']==sid,'page session scope')
        for c in page['candidates']:valid('ScopedCandidate',c)
        return candidate_evidence(page)
    def verify(result,payload):
        nonlocal exact
        check(result['status']=='completed' and Path(result['path']).read_bytes()==payload,result)
        check(result['sha256']==hashlib.sha256(payload).hexdigest(),result);exact+=1
    with tempfile.TemporaryDirectory(prefix='rescue-source-contract-') as temporary:
        root=Path(temporary);source=root/'original.raw';truth=build(source)
        before=hashlib.sha256(source.read_bytes()).hexdigest();info=inspect(source)
        check(info['kind']=='image' and info['read_map'] is None,info)
        renamed=root/'renamed.raw';source.rename(renamed)
        check(inspect(renamed)==info,'rename changed opaque source identity')
        link=root/'hard-link.raw';os.link(renamed,link)
        check(inspect(link)==info,'hard link changed source identity')
        clone=root/'copy.raw';shutil.copy2(renamed,clone)
        check(inspect(clone)['id']!=info['id'],'distinct local objects collapsed to one source ID')
        checks.append('names_and_hardlinks_preserve_object_id_but_copies_are_distinct')
        saved=run(binary,'scan','--source',renamed,'--work-dir',root/'legacy','--json')
        path=Path(saved['session']);session=json.loads(path.read_text(encoding='utf-8'));valid('Session',session)
        check(session['source_info']==info,'scan/inspect source mismatch')
        check(rows(path,session['id'])==session['candidates'],'scoped CLI rows changed metadata')
        indexed=run(binary,'scan','--indexed','--source',renamed,'--work-dir',root/'index','--json')
        header=run(binary,'report','--session',indexed['session'],'--json')['session'];valid('Session',header)
        check(header['source_info']==info and header['id']!=session['id'],'source and session identities conflated')
        check(rows(indexed['session'],header['id'])==session['candidates'],'indexed wire evidence mismatch')
        checks.append('json_index_and_wire_candidate_scopes_are_explicit')
        copied=run(binary,'index-import','--session',path,'--work-dir',root/'import','--json')
        copied_header=run(binary,'report','--session',copied['session'],'--json')['session']
        check(copied_header['id']!=session['id'] and copied_header['source_info']==info,'migration changed source identity')
        check(rows(copied['session'],copied_header['id'])==session['candidates'],'migration changed candidate evidence')
        checks.append('copy_migration_changes_session_scope_and_preserves_source')
        selected=next(iter(truth));payload=bytes.fromhex(truth[selected]['hex'])
        for streaming in [False,True]:
            args=['--stream'] if streaming else []
            restored=run(binary,'recover','--session',path,'--id',selected,'--destination',root/('report-out-'+str(streaming)),*args,'--json')
            verify(restored['results'][0],payload)
            check(json.loads(Path(restored['report']).read_text(encoding='utf-8'))['source_info']==info,'folder report lost source identity')
            archive=root/('report-'+str(streaming)+'.zip')
            run(binary,'recover','--session',path,'--id',selected,'--archive',archive,*args,'--json')
            with zipfile.ZipFile(archive) as z:
                check(z.testzip() is None,'ZIP checksum')
                report=json.loads(z.read('rescue-report.json'));check(report['source_info']==info,'ZIP report lost source identity')
                check(len(report['results'])==1 and z.read(report['results'][0]['archive_entry'])==payload,'ZIP payload');exact+=1
        checks.append('folder_and_zip_reports_keep_source_context_in_both_save_modes')
        legacy=json.loads(path.read_text(encoding='utf-8'));legacy.pop('source_info');old=root/'old.json';old.write_text(json.dumps(legacy),encoding='utf-8')
        result=run(binary,'recover','--session',old,'--id',selected,'--destination',root/'legacy-out','--json')['results'][0];verify(result,payload)
        checks.append('legacy_source_without_new_identity_still_revalidates_and_recovers')
        for label,change in [('id',lambda s:s['source_info'].update(id='src-'+'0'*64)),('window',lambda s:s['source_info'].update(offset=512)),('copy',lambda s:s.update(source=str(clone)))]:
            changed=json.loads(path.read_text(encoding='utf-8'));change(changed);edited=root/(label+'.json');edited.write_text(json.dumps(changed),encoding='utf-8')
            result=run(binary,'recover','--session',edited,'--id',selected,'--destination',root/(label+'-out'),'--json',expected=4)
            if isinstance(result,str):result=json.loads(result)
            check(result['error']['code']=='SOURCE_CHANGED' if 'error' in result else all(r['status']=='failed' and r['path'] is None for r in result['results']),result)
            check(not (root/(label+'-out')).exists(),'invalid identity created output')
        checks.append('forged_id_window_and_identical_but_different_source_rejected')
        disk=root/'disk.raw';disk_image(disk,[renamed],512,'mbr')
        whole=run(binary,'scan','--source',disk,'--work-dir',root/'whole','--json')
        result=run(binary,'recover','--session',whole['session'],'--id','p1:'+selected,'--destination',root/'whole-out','--json')['results'][0];verify(result,payload)
        checks.append('whole_disk_identity_remains_valid_for_partition_reads')
        lost=root/'lost-table.raw';shutil.copyfile(disk,lost);lost_info=inspect(lost)
        with lost.open('r+b') as f:f.write(bytes(512))
        discovered=run(binary,'discover','--source',lost,'--json')['volumes'][0]
        recovered=run(binary,'scan','--indexed','--source',lost,'--volume-id',discovered['id'],'--work-dir',root/'lost-work','--json')
        bounded=run(binary,'report','--session',recovered['session'],'--json')['session'];valid('Session',bounded)
        check(bounded['source_info']['offset']==discovered['region']['offset'] and bounded['source_info']['length']==discovered['region']['length'],'recovered source lost its bounded view')
        check(bounded['source_info']['bytes']==lost.stat().st_size and bounded['source_info']['id']==lost_info['id'],'recovered volume changed source identity')
        result=run(binary,'recover','--session',recovered['session'],'--id',selected,'--destination',root/'lost-out','--json')['results'][0];verify(result,payload)
        checks.append('indexed_discovered_volume_keeps_source_id_and_bounded_window')
        acquired=run(binary,'image','--source',renamed,'--destination',root/'acquired','--json')
        acquired_info=inspect(acquired['image'])
        check(acquired_info['kind']=='acquisition' and acquired_info['read_map']['summary']['copied_bytes']==info['bytes'],acquired_info)
        check(acquired_info['read_map']['original_source']['identity']==info['identity'],'acquisition lost original object identity')
        mapped=run(binary,'scan','--source',acquired['image'],'--work-dir',root/'mapped-work','--json')
        mapped_header=json.loads(Path(mapped['session']).read_text(encoding='utf-8'));check(mapped_header['source_info']==acquired_info,'mapped source mismatch')
        result=run(binary,'recover','--session',mapped['session'],'--id',selected,'--destination',root/'mapped-out','--json')['results'][0];verify(result,payload)
        moved=root/'acquired-copy';shutil.copytree(root/'acquired',moved)
        check(inspect(moved/'image.rescue.raw')==acquired_info,'sealed acquisition copy changed logical acquisition identity')
        checks.append('acquisition_id_origin_read_map_and_exact_recovery')
        with closing(Client(binary,renamed,root/'mcp-work',root/'mcp-out')) as client:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            for indexed_mode in [False,True]:
                done=client.wait(client.tool('start_scan',{'source':str(renamed),'indexed':indexed_mode,'request_id':'scan-'+str(indexed_mode)})['job_id'])
                sid=done['result']['session_id'];page=client.tool('list_candidates',{'session_id':sid,'limit':1000})
                check(page['session_id']==sid and candidate_evidence(page)==session['candidates'],'MCP candidate scope/evidence mismatch')
                for c in page['candidates']:valid('ScopedCandidate',c)
                report=client.tool('get_report',{'session_id':sid});reported=report.get('session',report)
                check(reported['source_info']==info,'MCP source information absent')
                wrong={'session_id':session['id'],'id':selected};valid('CandidateReference',wrong)
                rejected=client.tool('recover_files',{'session_id':sid,'ids':[wrong],'destination':str(root/'mcp-out/forbidden'),'request_id':'wrong-'+str(indexed_mode)},error=True)
                check(rejected['structuredContent']['error']['code']=='INVALID_REQUEST' and not (root/'mcp-out/forbidden').exists(),'foreign candidate scope accepted')
                reference={'session_id':sid,'id':selected}
                done=client.wait(client.tool('recover_files',{'session_id':sid,'ids':[reference],'destination':str(root/('mcp-out/'+str(indexed_mode))),'request_id':'correct-'+str(indexed_mode)})['job_id'])
                verify(done['result']['results'][0],payload)
        checks.append('mcp_explicit_candidate_references_reject_foreign_sessions')
        check(hashlib.sha256(renamed.read_bytes()).hexdigest()==before,'source changed')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'schema_validated_records':validated,'source_unchanged':True,'scope':__doc__.strip()}
    if output:
        output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output.resolve() if a.output else None)
