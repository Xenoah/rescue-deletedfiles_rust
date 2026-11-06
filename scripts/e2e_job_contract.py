"""Real pause/resume, copy migration and typed errors against the public schema."""
import argparse,json,shutil,sqlite3,subprocess,tempfile,time
from pathlib import Path
import jsonschema
from e2e import Client,check,run
from make_fixture import build
from make_fat_fixture import file_hash


def main(binary,output=None):
    schema=json.loads((Path(__file__).resolve().parent.parent/'schemas/v1.json').read_text())
    jsonschema.Draft202012Validator.check_schema(schema)
    checks=[];exact=0;validated=0
    def valid(name,value):
        nonlocal validated
        jsonschema.Draft202012Validator({'$ref':'#/$defs/'+name,'$defs':schema['$defs']}).validate(value);validated+=1
    def error(value):
        value=json.loads(value) if isinstance(value,str) else value
        valid('OperationError',value['error']);return value['error']
    def wait(client,job_id):
        end=time.monotonic()+90
        while time.monotonic()<end:
            job=client.tool('get_job',{'job_id':job_id});valid('Job',job)
            if job['state'] not in ['queued','running','pausing','cancelling']:return job
            time.sleep(.01)
        raise AssertionError('job did not reach a terminal state')
    with tempfile.TemporaryDirectory(prefix='rescue-contract-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth=build(source)
        # A few thousand durable blocks give the real client time to pause,
        # without a production slowdown hook or a synthetic state transition.
        with source.open('r+b') as f:f.truncate(64*1024*1024)
        before=file_hash(source);work=root/'work';work.mkdir();out=root/'out'
        layout=run(binary,'inspect','--source',source,'--json')['layout'];valid('Source',layout)
        old=run(binary,'scan','--source',source,'--work-dir',work/'legacy','--json')
        legacy=Path(old['session']);header=json.loads(legacy.read_text(encoding='utf-8'));valid('Session',header)
        for c in header['candidates']:valid('Candidate',c)
        legacy_hash=file_hash(legacy)
        malformed=run(binary,'scan','--source',source,'--mode','carve','--max-depth','1','--work-dir',work/'invalid','--json',expected=2)
        check(error(malformed)['code']=='INVALID_REQUEST','CLI error code changed')
        checks.append('public_source_session_candidate_and_cli_error_schema')
        client=Client(binary,source,work,out)
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            denied=client.tool('get_job',{'job_id':'unknown'},error=True)
            check(error(denied['structuredContent'])['affected_scope']=='tool:get_job','tool error scope')
            checks.append('typed_tool_error_preserves_numeric_rpc_layer')
            image_args={'source':str(source),'destination':str(out/'acquisition'),'block_size':16*1024,'retries':0,'request_id':'image'}
            started=client.tool('start_image',image_args)
            check(client.tool('start_image',image_args)==started,'image duplicate created a job')
            deadline=time.monotonic()+20
            while True:
                job=client.tool('get_job',{'job_id':started['job_id']});valid('Job',job)
                check(job['state']=='running','image completed before actual pause')
                if job['details']['pause_supported'] and job['progress']['scanned_records']>0:break
                check(time.monotonic()<deadline,'no durable imaging progress');time.sleep(.005)
            requested=client.tool('pause_job',{'job_id':started['job_id']});valid('Job',requested)
            check(requested['state']=='pausing','pause acknowledged as completed too soon')
            paused=wait(client,started['job_id']);check(paused['state']=='paused',paused)
            check(paused['kind']=='imaging' and paused['details']['resume_mode']=='durable_read_map',paused)
            check(0<paused['result']['summary']['copied_bytes']<source.stat().st_size,'pause is not partial')
            session_id=paused['result']['session_id'];image_path=Path(paused['result']['image'])
            resume_args={'session_id':session_id,'request_id':'resume-image'}
            resumed=client.tool('resume_job',resume_args)
            check(resumed['job_id']!=started['job_id'] and client.tool('resume_job',resume_args)==resumed,'resume lost history or dedup')
            done=wait(client,resumed['job_id']);check(done['state']=='completed',done)
            check(file_hash(image_path)==before,'resumed acquisition differs byte for byte')
            check(client.tool('get_job',{'job_id':started['job_id']})==paused,'old job history mutated')
            check(client.tool('pause_job',{'job_id':resumed['job_id']})==done,'terminal pause changed result')
            checks.extend(['actual_imaging_pause_after_durable_block','resume_new_job_dedup_and_immutable_history','resumed_image_full_hash'])
            copied=wait(client,client.tool('import_session',{'path':str(legacy),'request_id':'import-json'})['job_id'])
            check(copied['state']=='completed' and copied['kind']=='session_migration',copied)
            check(not copied['details']['pause_supported'] and bool(copied['details']['pause_reason']),'migration pause contract')
            sid=copied['result']['session_id'];check(sid!=header['id'],'copy reused original session ID')
            check(file_hash(legacy)==legacy_hash,'import changed original JSON')
            rows=client.tool('list_candidates',{'session_id':sid,'limit':100})['candidates']
            check(rows==header['candidates'],'migration changed evidence')
            recovered=wait(client,client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(out/'restored'),'request_id':'recover-copy'})['job_id'])
            check(recovered['state']=='completed',recovered)
            for row in recovered['result']['results']:
                valid('RecoveryResult',row);check(Path(row['path']).read_bytes().hex()==truth[row['candidate_id']]['hex'],'migrated recovery bytes');exact+=1
            checks.append('mcp_json_copy_new_identity_unchanged_original_and_recovery')
            # The original remains available while its second copy is registered.
            path=Path(copied['result']['session_file']);earlier=work/'schema1.sqlite'
            db=sqlite3.connect(path);copy=sqlite3.connect(earlier);db.backup(copy);db.close()
            copy.executescript('ALTER TABLE candidate RENAME TO candidate_v2; CREATE TABLE candidate AS SELECT seq,id,payload,discovery_key,name_key,path_key,size_key,created_key,modified_key,search_text,supported FROM candidate_v2; DROP TABLE candidate_v2; PRAGMA user_version=1;');copy.close()
            earlier_hash=file_hash(earlier)
            migrated=wait(client,client.tool('import_session',{'path':str(earlier),'request_id':'import-schema1'})['job_id'])
            check(migrated['state']=='completed' and migrated['result']['session_id']!=sid,migrated)
            check(file_hash(earlier)==earlier_hash,'old SQLite was edited')
            check(client.tool('list_candidates',{'session_id':migrated['result']['session_id']})['candidates']==rows,'old SQLite evidence changed')
            check(client.tool('list_candidates',{'session_id':sid})['candidates']==rows,'original copy registration lost')
            checks.append('old_sqlite_copy_preserves_both_sessions_and_original_bytes')
            outside=root/'outside.json';shutil.copyfile(legacy,outside)
            denied=client.tool('import_session',{'path':str(outside),'request_id':'outside'},error=True);error(denied['structuredContent'])
            tampered=json.loads(legacy.read_text(encoding='utf-8'));tampered['source']=str(root/'forbidden.raw')
            forged=work/'forged.json';forged.write_text(json.dumps(tampered),encoding='utf-8')
            count=len(list(work.glob('*.rescue-index')))
            failed=wait(client,client.tool('import_session',{'path':str(forged),'request_id':'forged'})['job_id'])
            check(failed['state']=='failed' and error(failed['result'])['code']=='INVALID_REQUEST',failed)
            check(len(list(work.glob('*.rescue-index')))==count,'unauthorized source caused a new index')
            checks.append('migration_work_and_source_confinement_typed_async_error')
            # A preview failure exposes the same machine contract as CLI.
            unsupported=wait(client,client.tool('preview_candidate',{'session_id':sid,'candidate_id':'mft-21-7','destination':str(out/'preview'),'request_id':'unsupported'})['job_id'])
            check(unsupported['state']=='failed',unsupported);error(unsupported['result'])
            checks.append('preview_failure_uses_shared_error_contract')
            # Restart uses explicit original source/destination; never guesses
            # an acquisition from an arbitrary read-map path outside permissions.
            final_image=image_path
        finally:client.close()
        client=Client(binary,source,work,out)
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            reopened=wait(client,client.tool('start_image',{**image_args,'resume':True,'request_id':'after-restart'})['job_id'])
            check(reopened['state']=='completed' and file_hash(final_image)==before,'restart acquisition differs')
            check(reopened['result']['session_id']==session_id,'acquisition checkpoint ID changed')
            checks.append('acquisition_identity_survives_server_restart')
        finally:client.close()
        # Legacy recovery error text remains; structured fields are additive.
        forged=json.loads(legacy.read_text(encoding='utf-8'));forged['candidates'][0]['record_sha256']='0'*64
        path=work/'changed-candidate.json';path.write_text(json.dumps(forged),encoding='utf-8')
        failed=run(binary,'recover','--session',path,'--id',forged['candidates'][0]['id'],'--destination',out/'bad','--json',expected=5)['results'][0]
        valid('RecoveryResult',failed);valid('OperationError',failed['error_details'])
        check(failed['error_details']['code']=='SOURCE_CHANGED' and not failed['error_details']['retryable'] and failed['path'] is None,'incorrect recovery error contract')
        checks.append('per_candidate_error_keeps_legacy_text_and_blocks_output')
        check(file_hash(source)==before and file_hash(legacy)==legacy_hash,'source or original session changed')
        # Cancel a real, sizeable migration while its source is held as a
        # read snapshot. The unfinished copy must not become a resumable job.
        from make_large_fixture import build as large_fixture
        large=root/'large.raw';large_fixture(large,20000);large_before=file_hash(large)
        large_work=root/'large-work'
        scanned=run(binary,'scan','--indexed','--source',large,'--work-dir',large_work,'--json',timeout=90)
        original=Path(scanned['session']);original_before=file_hash(original)
        client=Client(binary,large,large_work,root/'large-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            started=client.tool('import_session',{'path':str(original),'request_id':'cancel-copy'})
            stopping=client.tool('cancel_job',{'job_id':started['job_id']});valid('Job',stopping)
            check(stopping['state']=='cancelling','migration completed before cancellation test')
            stopped=wait(client,started['job_id']);check(stopped['state']=='cancelled',stopped)
            check(error(stopped['result'])['code']=='CANCELLED' and 'session_id' not in stopped['result'],'partial migration published')
            check(not stopped['details']['pause_supported'],'unfinished migration advertised resume')
        finally:client.close()
        check(file_hash(original)==original_before and file_hash(large)==large_before,'cancelled migration modified input')
        checks.append('actual_copy_cancellation_never_publishes_a_partial_session')
    proof={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'schema_validated_records':validated,'files_byte_exact':exact,'source_unchanged':True}
    if output:Path(output).write_text(json.dumps(proof,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(proof),flush=True);return proof


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
