"""Explicit exFAT upcase-content damage tolerance with strict allocation/ownership.

Synthetic images only. Valid entry-set checksums and UTF-16 names are retained;
missing name-hash verification never becomes verified path evidence.
"""
import argparse,contextlib,csv,hashlib,io,json,sqlite3,struct,subprocess,tempfile,zipfile
from pathlib import Path
import jsonschema
from e2e import Client,check,run,candidate_evidence
from e2e_fat import write_at,modify_set
from e2e_index_fat import large
from e2e_disks import disk_image
from e2e_reports import Document
from make_fat_fixture import exfat,exfat_set,file_hash,rotate_sum
FLAG='--exfat-allow-damaged-upcase'
MARK='EXFAT_UPCASE_UNVERIFIED:'


def pages(binary,path):
    if str(path).endswith('.json'):return json.loads(Path(path).read_text(encoding='utf-8'))['candidates']
    result=[];cursor=None
    while True:
        args=['results','--session',path,'--limit','1000','--json']
        if cursor:args+=['--cursor',json.dumps(cursor)]
        page=run(binary,*args);result+=candidate_evidence(page);cursor=page.get('next_cursor')
        if cursor is None:return result


def damaged(path,kind='checksum'):
    truth,unsupported,meta=exfat(path)
    at=meta['heap']+2*meta['cluster']
    if kind=='checksum':write_at(path,at+100,b'\x55')
    else:
        write_at(path,at,b'\xff\xff\x00\x00')
        with path.open('rb') as f:f.seek(at);data=f.read(131072)
        write_at(path,meta['heap']+32+4,struct.pack('<I',rotate_sum(data,32)))
    return truth,unsupported,meta


def main(binary,output=None):
    checks=[];exact=0
    def recovered(rows,truth,archive=None):
        nonlocal exact
        check(len(rows)==len(truth),'unexpected recovered count')
        for row in rows:
            data=archive.read(row['archive_entry']) if archive else Path(row['path']).read_bytes()
            key=row['candidate_id'].split(':',1)[-1]
            check(row['status']=='completed' and data==truth[key] and row['sha256']==hashlib.sha256(data).hexdigest(),'recovered bytes')
            check(any(MARK in w for w in row['warnings']),'missing name-hash warning in recovery')
            exact+=1
    with tempfile.TemporaryDirectory(prefix='rescue-exfat-damage-') as temporary:
        root=Path(temporary)
        for kind in ['checksum','encoding']:
            source=root/f'{kind}.raw';truth,unsupported,meta=damaged(source,kind);before=file_hash(source)
            run(binary,'scan','--source',source,'--work-dir',root/f'{kind}-strict','--json',expected=4)
            checks.append(kind+'_strict_default_refuses_damage')
            for mode,extra in [('json',[]),('index',['--indexed'])]:
                scanned=run(binary,'scan','--source',source,'--work-dir',root/f'{kind}-{mode}',FLAG,*extra,'--json')
                rows=pages(binary,scanned['session']);by_id={r['id']:r for r in rows}
                check(set(by_id)==set(truth)|set(unsupported),'candidate set changed')
                check({r['id'] for r in rows if r['supported']}==set(truth),'unrecoverable data accepted')
                check(all(not r['path_verified'] and any(MARK in w for w in r['warnings']) for r in rows),'unverified name claimed as verified')
                saved=run(binary,'report','--session',scanned['session'],'--json');header=saved['session'] if extra else json.loads(Path(scanned['session']).read_text(encoding='utf-8'))
                check(header['schema_version']==11 and header['search']['exfat_allow_damaged_upcase'],'feature version missing')
                check(any(MARK in w for w in header['warnings']),'missing volume warning')
                schema=json.loads((Path(__file__).resolve().parent.parent/'schemas/v1.json').read_text())
                jsonschema.validate(header,{'$schema':schema['$schema'],'$ref':'#/$defs/Session','$defs':schema['$defs']})
                result=run(binary,'recover','--session',scanned['session'],'--all','--destination',root/f'{kind}-{mode}-out','--json');recovered(result['results'],truth)
                if mode=='json':expected=rows;json_path=Path(scanned['session']);report=Path(result['report'])
                else:check(rows==expected,'indexed interpretation differs');indexed=scanned['session']
            checks.extend([kind+'_json_index_paths_and_bytes',kind+'_allocation_conflicts_and_bad_chains_still_unsupported'])
            archive=root/f'{kind}.zip';run(binary,'recover','--session',indexed,'--all','--archive',archive,'--json')
            with zipfile.ZipFile(archive) as z:
                check(z.testzip() is None,'ZIP CRC');recovered(json.loads(z.read('rescue-report.json'))['results'],truth,z)
            checks.append(kind+'_zip_preserves_bytes_and_uncertainty')
            original=json.loads(report.read_text(encoding='utf-8'))
            for format in ['html','csv']:
                out=root/f'{kind}.{format}';run(binary,'export-report','--input',report,'--output',out,'--format',format,'--json')
                if format=='html':document=Document();document.feed(out.read_text(encoding='utf-8'));values=[json.loads(p) for p in document.pre[:-1]]
                else:values=[json.loads(row['result_json']) for row in csv.DictReader(io.StringIO(out.read_text(encoding='utf-8-sig')))]
                check(values==original['results'],'readable report changed evidence')
            checks.append(kind+'_html_csv_preserve_missing_name_hash_evidence')
            key=f"exfat-{meta['heap']+(200-2)*meta['cluster']}"
            preview=run(binary,'preview','--session',indexed,'--id',key,'--destination',root/f'{kind}-preview','--json')
            check(Path(preview['path']).read_bytes()==truth[key],'preview did not retain opt-in')
            checks.append(kind+'_preview_revalidates_same_setting')
            feature=json.loads(json_path.read_text(encoding='utf-8'))
            for changed in [{'schema_version':9},{'schema_version':10}]:
                bad=root/f'{kind}-schema{changed["schema_version"]}.json';bad.write_text(json.dumps(dict(feature,**changed)),encoding='utf-8')
                run(binary,'recover','--session',bad,'--all','--destination',root/'denied','--json',expected=4)
                check(not (root/'denied').exists(),'schema downgrade wrote output')
            feature['search']['exfat_allow_damaged_upcase']=False;bad=root/f'{kind}-missing-opt-in.json';bad.write_text(json.dumps(feature),encoding='utf-8')
            run(binary,'report','--session',bad,'--json',expected=4)
            checks.append(kind+'_schema_and_setting_mismatch_refused')
            check(file_hash(source)==before,'source changed')
        source=root/'checksum.raw';truth,_,meta=damaged(root/'mcp.raw');mcp_source=root/'mcp.raw';before=file_hash(mcp_source)
        client=Client(binary,mcp_source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            client.tool('start_scan',{'source':str(mcp_source),'exfat_allow_damaged_upcase':'true','request_id':'bad'},error=True)
            job=client.wait(client.tool('start_scan',{'source':str(mcp_source),'indexed':True,'exfat_allow_damaged_upcase':True,'request_id':'scan'})['job_id']);check(job['state']=='completed',job);sid=job['result']['session_id']
            rows=candidate_evidence(client.tool('list_candidates',{'session_id':sid,'limit':100}));check(all(not row['path_verified'] and any(MARK in w for w in row['warnings']) for row in rows),'MCP lost warning')
            job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(root/'mcp-out'),'request_id':'recover'})['job_id']);check(job['state']=='completed',job);recovered(job['result']['results'],truth)
        finally:client.close()
        check(file_hash(mcp_source)==before,'MCP modified source');checks.append('mcp_typed_opt_in_evidence_and_recovery')
        for scheme,extra,sector in [('mbr',[],512),('gpt',['--indexed'],4096)]:
            disk=root/f'{scheme}.raw';disk_image(disk,[source],sector,scheme);before=file_hash(disk)
            scan=run(binary,'scan','--source',disk,'--work-dir',root/scheme,FLAG,*extra,'--json')
            rows=pages(binary,scan['session']);check(all(not row['path_verified'] for row in rows),'partition names promoted')
            result=run(binary,'recover','--session',scan['session'],'--all','--destination',root/f'{scheme}-out','--json');recovered(result['results'],truth)
            check(file_hash(disk)==before,'disk modified');checks.append(scheme+'_partition_settings_and_byte_exact_recovery')
        # The opt-in has no effect when the table is valid; mismatching name hashes
        # remain errors. Metadata extents, bitmap and entry checksums never relax.
        good=root/'good.raw';_,_,meta=exfat(good)
        plain=run(binary,'scan','--source',good,'--work-dir',root/'good-default','--json')
        allowed=run(binary,'scan','--source',good,'--work-dir',root/'good-opt-in',FLAG,'--fat-short-name-encoding','cp932','--json')
        check(pages(binary,plain['session'])==pages(binary,allowed['session']),'valid table behavior changed')
        check(json.loads(Path(allowed['session']).read_text(encoding='utf-8'))['schema_version']==11,'combined settings schema')
        checks.append('intact_upcase_unchanged_and_cp932_combination_schema')
        modify_set(good,meta['positions']['連続配置.bin'],lambda data:struct.pack_into('<H',data,36,0))
        result=run(binary,'scan','--source',good,'--work-dir',root/'bad-name-hash',FLAG,'--json',expected=5)
        check(any('name hash mismatch' in w for w in result['warnings']),'intact table name hash bypassed');checks.append('valid_upcase_still_checks_name_hash')
        for kind in ['missing_table','duplicate_table','table_size','table_chain','bitmap_free_metadata','missing_bitmap']:
            path=root/f'{kind}.raw';_,_,meta=damaged(path);h=meta['heap']
            if kind=='missing_table':write_at(path,h+32,b'\xa0')
            elif kind=='duplicate_table':
                with path.open('rb') as f:f.seek(h+32);entry=f.read(32)
                write_at(path,h+64,entry)
            elif kind=='table_size':write_at(path,h+32+24,struct.pack('<Q',131074))
            elif kind=='table_chain':write_at(path,meta['fat']+4*4,struct.pack('<I',0xffffffff))
            elif kind=='bitmap_free_metadata':write_at(path,meta['bitmap'],b'\0')
            else:write_at(path,h,b'\xa0')
            before=file_hash(path);run(binary,'scan','--source',path,'--work-dir',root/f'{kind}-work',FLAG,'--json',expected=4);check(file_hash(path)==before,'rejected source changed');checks.append(kind+'_refused_even_with_opt_in')
        path=root/'bad-entry.raw';_,_,meta=damaged(path);offset=meta['positions']['連続配置.bin'];write_at(path,offset+8,b'\x55')
        result=run(binary,'scan','--source',path,'--work-dir',root/'bad-entry',FLAG,'--json',expected=5)
        check(f'exfat-{offset}' not in {r['id'] for r in pages(binary,result['session'])},'bad entry checksum accepted');checks.append('damaged_entry_checksum_still_rejected')
        path=root/'live-owner.raw';_,_,meta=damaged(path);slot=meta['heap']+(40-2)*meta['cluster']+3*32
        write_at(path,slot,b''.join(exfat_set('LiveOwner.bin',190,24,deleted=False)))
        result=run(binary,'scan','--source',path,'--work-dir',root/'live-owner',FLAG,'--json')
        key=f"exfat-{meta['positions']['single-cleared.bin']}";row=next(r for r in pages(binary,result['session']) if r['id']==key)
        check(not row['supported'],'live owner bypassed by damaged table');checks.append('live_ownership_conflict_remains_unsupported')
        path=root/'bad-utf16.raw';_,_,meta=damaged(path)
        modify_set(path,meta['positions']['連続配置.bin'],lambda b:struct.pack_into('<H',b,66,0xd800))
        result=run(binary,'scan','--source',path,'--work-dir',root/'bad-utf16',FLAG,'--json',expected=5)
        check(any('UTF-16' in w for w in result['warnings']),'invalid UTF-16 accepted');checks.append('invalid_utf16_still_rejected')
        # Crash a real traversal, reject an option mismatch, then resume untouched evidence.
        path=root/'large.raw';large(path,'exfat',damaged);before=file_hash(path)
        scan=run(binary,'scan','--source',path,'--work-dir',root/'full',FLAG,'--json');expected=pages(binary,scan['session'])
        child=subprocess.Popen([str(binary),'scan','--indexed','--source',str(path),'--work-dir',str(root/'crash'),FLAG,'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        try:
            while True:
                line=child.stderr.readline();check(bool(line),'no checkpoint progress');event=json.loads(line)
                if event['progress']['scanned_records']>=128:break
            index=Path(event['index']);child.kill();child.communicate(timeout=20)
        finally:
            if child.poll() is None:child.kill();child.wait(timeout=20)
        with contextlib.closing(sqlite3.connect(index)) as db:
            saved=db.execute('SELECT engine_state FROM checkpoint').fetchone()[0];state=json.loads(saved);check(state['options']['exfat_allow_damaged_upcase'],'checkpoint option absent')
            state['options']['exfat_allow_damaged_upcase']=False;db.execute('UPDATE checkpoint SET engine_state=?',(json.dumps(state),));db.commit()
        run(binary,'resume','--session',index,'--json',expected=2)
        with contextlib.closing(sqlite3.connect(index)) as db:db.execute('UPDATE checkpoint SET engine_state=?',(saved,));db.commit()
        check(run(binary,'resume','--session',index,'--json')['index']['state']=='completed','resume failed');check(pages(binary,index)==expected,'resume lost metadata/uncertainty');check(file_hash(path)==before,'large source changed')
        checks.extend(['checkpoint_setting_mismatch_refused','actual_process_kill_resume_preserves_missing_evidence'])
        run(binary,'scan','--source',path,'--work-dir',root/'carve',FLAG,'--mode','carve','--json',expected=2);checks.append('carving_rejects_metadata_only_option')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
