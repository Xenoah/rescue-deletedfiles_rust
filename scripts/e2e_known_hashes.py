"""Known SHA-256 comparison of real recovery records, with honest recorded-digest scope.

Content matches never certify an original name, remove uncertainty, or imply that
saved files were re-read. Manifest labels are never used as filesystem paths.
"""
import argparse,copy,hashlib,json,subprocess,tempfile,zipfile
from pathlib import Path
import jsonschema
from e2e import Client,check,run
from e2e_cp932 import make
from make_fat_fixture import file_hash


def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-known-hashes-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth,_,_=make(source,16);before=file_hash(source)
        scan=run(binary,'scan','--source',source,'--work-dir',root/'work','--fat-short-name-encoding','cp932','--json')
        restored=run(binary,'recover','--session',scan['session'],'--all','--destination',root/'out','--json');report=Path(restored['report']);original=json.loads(report.read_text(encoding='utf-8'));report_hash=file_hash(report)
        for row in original['results']:
            check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id']],'recovery bytes');exact+=1
        selected=original['results'][:2];h1,h2=[r['sha256'] for r in selected]
        raw=('\ufeff'+h1.upper()+'  日本/元の写真.txt\r\n'+h1+' *同じ内容の別名.txt\r\n'+h2+'  ../../never-open-this-label\r\n'+'01'*32+'\r\n').encode('utf-8')
        manifest=root/'SHA256SUMS.txt';manifest.write_bytes(raw);manifest_hash=file_hash(manifest)
        def compare(path=report,list_path=manifest,dest=None):
            args=['match-hashes','--report',path,'--checksums',list_path,'--json']
            if dest:args+=['--output',dest]
            return run(binary,*args)['comparison']
        def verify(comparison,expected):
            check(comparison['counts']=={'matched':2,'not_listed':6,'not_comparable':0},comparison['counts'])
            check(comparison['recovery_report']==expected,'original source, names or evidence changed')
            check(comparison['manifest_sha256']==manifest_hash and comparison['manifest_entries']==4,'manifest provenance')
            check(comparison['known_contents'][h1]==['日本/元の写真.txt','同じ内容の別名.txt'],'duplicate content names lost')
            check(comparison['comparison_basis']=='sha256_recorded_when_recovery_wrote_output' and 'not re-read' in comparison['integrity_note'],'comparison scope unclear')
            check({r['candidate_id'] for r in comparison['results'] if r['outcome']=='matched'}=={r['candidate_id'] for r in selected},'wrong content match')
        destination=root/'comparison.json';comparison=compare(dest=destination);verify(comparison,original)
        check(json.loads(destination.read_text(encoding='utf-8'))==comparison,'saved comparison differs')
        schema=json.loads((Path(__file__).resolve().parent.parent/'schemas/v1.json').read_text())
        jsonschema.validate(comparison,{'$schema':schema['$schema'],'$ref':'#/$defs/KnownHashComparison','$defs':schema['$defs']})
        checks+=['real_cli_recovery_byte_exact','unicode_bom_crlf_text_binary_and_bare_hash_lines','known_matches_and_not_listed_are_distinct','duplicate_content_labels_do_not_choose_an_original_name','manifest_digest_and_full_original_evidence_retained','separate_saved_comparison_matches_json_schema']
        digest=file_hash(destination)
        process=subprocess.run([str(binary),'match-hashes','--report',str(report),'--checksums',str(manifest),'--output',str(destination),'--json'],capture_output=True)
        check(process.returncode!=0 and file_hash(destination)==digest,'comparison overwritten');checks.append('existing_output_never_overwritten')
        # Report and manifest path labels are data, not read access requests.
        edited=copy.deepcopy(original)
        for row in edited['results']:row['path']=str(root/'nonexistent-secret');row['archive_entry']='../../not-followed'
        edited_path=root/'opaque-paths.json';edited_path.write_text(json.dumps(edited),encoding='utf-8');verify(compare(edited_path),edited);checks.append('report_paths_and_manifest_labels_never_followed')
        # Deliberately alter a saved output: comparison still describes the recorded
        # digest, and explicitly does not claim a fresh content verification.
        changed=Path(selected[0]['path']);changed.write_bytes(b'changed after recovery')
        check(hashlib.sha256(changed.read_bytes()).hexdigest()!=h1,'fixture change ineffective');verify(compare(),original)
        changed.write_bytes(truth[selected[0]['candidate_id']]);checks.append('recorded_digest_scope_remains_explicit_after_saved_file_change')
        incomplete=copy.deepcopy(original)
        incomplete['results'][0]['status']='partial'
        incomplete['results'][1]['missing_ranges']=[{'source_offset':512,'length':8,'reason':'failed'}]
        incomplete['results'][2]['sha256']=None
        incomplete['results'][3]['error']='read failed'
        p=root/'incomplete.json';p.write_text(json.dumps(incomplete),encoding='utf-8');partial=compare(p)
        check(partial['counts']=={'matched':0,'not_listed':4,'not_comparable':4},partial['counts']);check(partial['recovery_report']==incomplete,'incomplete evidence changed')
        checks.append('partial_missing_ranges_errors_and_missing_hashes_not_compared')
        malformed=[b'',b'\xff',f'{h1} one-space'.encode(),f'{h1}  repeat\n{h1}  repeat'.encode(),f'{h1}  conflict\n{h2}  conflict'.encode(),f'\\{h1}  escaped\\nname'.encode(),f'{h1}  tab\tname'.encode(),('g'*64+'  invalid').encode(),(h1+'  '+'a'*4097).encode()]
        for i,data in enumerate(malformed):
            p=root/f'bad-list-{i}';p.write_bytes(data);out=root/f'bad-list-{i}.json'
            process=subprocess.run([str(binary),'match-hashes','--report',str(report),'--checksums',str(p),'--output',str(out),'--json'],capture_output=True)
            check(process.returncode!=0 and not out.exists(),'bad list accepted')
        checks.append('malformed_duplicate_conflicting_and_escaped_lists_refused')
        huge=root/'huge-list';huge.write_bytes(b' '*(4*1024*1024+1))
        many=root/'too-many';many.write_text(''.join(hashlib.sha256(str(i).encode()).hexdigest()+'  file'+str(i)+'\n' for i in range(20001)),encoding='utf-8')
        for p in [huge,many]:
            process=subprocess.run([str(binary),'match-hashes','--report',str(report),'--checksums',str(p),'--json'],capture_output=True);check(process.returncode!=0,'list resource bound bypassed')
        checks.append('manifest_byte_and_entry_limits_enforced')
        for label,data in [('schema',dict(original,schema_version=2)),('row',{'schema_version':1,'results':[{}]}),('duplicate-id',dict(original,results=[original['results'][0]]*2)),('bad-digest',dict(original,results=[dict(original['results'][0],sha256='bad')]))]:
            p=root/f'bad-report-{label}.json';p.write_text(json.dumps(data),encoding='utf-8');process=subprocess.run([str(binary),'match-hashes','--report',str(p),'--checksums',str(manifest),'--json'],capture_output=True);check(process.returncode!=0,'bad report accepted')
        p=root/'huge-report.json'
        with p.open('wb') as f:f.truncate(16*1024*1024+1)
        process=subprocess.run([str(binary),'match-hashes','--report',str(p),'--checksums',str(manifest),'--json'],capture_output=True);check(process.returncode!=0,'large report accepted');checks.append('report_schema_digest_unique_ids_and_size_validated')
        archive=root/'files.zip';run(binary,'recover','--session',scan['session'],'--all','--archive',archive,'--json')
        with zipfile.ZipFile(archive) as z:
            check(z.testzip() is None,'ZIP CRC');data=z.read('rescue-report.json');zip_report=json.loads(data)
            for row in zip_report['results']:check(z.read(row['archive_entry'])==truth[row['candidate_id']],'ZIP bytes');exact+=1
        extracted=root/'zip-report.json';extracted.write_bytes(data);verify(compare(extracted),zip_report);checks.append('zip_embedded_report_comparison_and_real_bytes')
        client=Client(binary,source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            scan_job=client.wait(client.tool('start_scan',{'source':str(source),'indexed':True,'fat_short_name_encoding':'cp932','request_id':'scan'})['job_id']);sid=scan_job['result']['session_id']
            client.tool('match_known_hashes',{'job_id':scan_job['job_id'],'checksums':raw.decode('utf-8')},error=True)
            for format in ['directory','zip']:
                dest=root/'mcp-out'/('folder' if format=='directory' else 'files.zip')
                job_id=client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(dest),'output_format':format,'request_id':'recover-'+format})['job_id'];job=client.wait(job_id);check(job['state']=='completed',job)
                rows=job['result']['results'] if format=='directory' else job['result']['archive']['results']
                if format=='directory':
                    for row in rows:check(Path(row['path']).read_bytes()==truth[row['candidate_id']],'MCP directory bytes');exact+=1
                else:
                    with zipfile.ZipFile(dest) as z:
                        for row in rows:check(z.read(row['archive_entry'])==truth[row['candidate_id']],'MCP ZIP bytes');exact+=1
                result=client.tool('match_known_hashes',{'job_id':job_id,'checksums':raw.decode('utf-8')});verify(result,{'schema_version':1,'recovery_job_id':job_id,'results':rows})
                client.tool('match_known_hashes',{'job_id':job_id,'checksums':'invalid'},error=True)
            checks+=['mcp_finished_directory_recovery_job_compared','mcp_finished_zip_recovery_job_compared','mcp_non_recovery_and_invalid_manifest_refused']
        finally:client.close()
        check(file_hash(source)==before and file_hash(report)==report_hash and file_hash(manifest)==manifest_hash,'source or evidence changed');checks.append('source_manifest_and_recovery_report_unchanged')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
