"""Selected recovery beyond former enumeration limits, through real interfaces."""
import argparse, hashlib, json, subprocess, tempfile, zipfile
from pathlib import Path
from e2e import run, check, Client
from make_large_fixture import build
from make_fat_fixture import fat32, exfat, file_hash
from e2e_index_fat import large


def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-late-') as temporary:
        root=Path(temporary)
        for kind in ['ntfs','fat32','exfat']:
            source=root/(kind+'.raw')
            if kind=='ntfs':
                oracle=build(source,100010,late_record=2_000_016)
                truth={key:bytes.fromhex(value['hex']) for key,value in oracle['examples'].items()}
                extension='txt'
            else:
                truth=large(source,kind,fat32 if kind=='fat32' else exfat,count=100010,tail=True)
                extension='prf'
            before=file_hash(source)
            scan=run(binary,'scan','--indexed','--source',source,'--work-dir',root/(kind+'-work'),
                     '--extension',extension,'--json',timeout=240)
            path=Path(scan['session'])
            page=run(binary,'results','--session',path,'--json')
            check(len(page['candidates'])==1 and page['candidates'][0]['supported'],page)
            candidate=page['candidates'][0];expected=truth[candidate['id']]
            # Source is independently regenerated; selection must work despite
            # >100K earlier deleted files, and NTFS's old 2M-record ceiling.
            zipped=run(binary,'recover','--session',path,'--id',candidate['id'],
                       '--archive',root/(kind+'.zip'),'--json',timeout=90)['result']
            with zipfile.ZipFile(zipped['archive']) as z:
                row=zipped['results'][0]
                check(row['status']=='completed' and z.read(row['archive_entry'])==expected,row)
            exact+=1
            client=Client(binary,source,root/(kind+'-work'),root/(kind+'-mcp'))
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                opened=client.tool('open_session',{'path':str(path)})
                job=client.tool('recover_files',{'session_id':opened['session_id'],'ids':[candidate['id']],
                    'destination':str(root/(kind+'-mcp')),'request_id':'late-recovery'})
                result=client.wait(job['job_id']);check(result['state']=='completed',result)
                row=result['result']['results'][0];check(Path(row['path']).read_bytes()==expected,row);exact+=1
            finally:client.close()
            check(file_hash(source)==before,'source bytes changed')
            if kind=='ntfs':
                full=run(binary,'scan','--indexed','--source',source,'--work-dir',root/'orders','--json',timeout=240)
                order_path=Path(full['session'])
                first=run(binary,'results','--session',order_path,'--group','folder','--sort','size','--descending','--limit','3','--json',timeout=90)
                check(len(first['candidates'])==3 and first['candidates'][0]['id']==candidate['id'],'compound order lost late target')
                second=run(binary,'results','--session',order_path,'--group','folder','--sort','size','--descending','--limit','3','--cursor',json.dumps(first['next_cursor']),'--json')
                check(not {c['id'] for c in first['candidates']} & {c['id'] for c in second['candidates']},'compound cursor duplicate')
                rare=run(binary,'results','--session',order_path,'--sort','name','--name','late-oracle','--json')
                common=run(binary,'results','--session',order_path,'--sort','name','--name','file-','--limit','500','--json')
                check(len(rare['candidates'])==1 and len(common['candidates'])==500,'adaptive trigram result mismatch')
                checks.extend(['large_compound_order_and_cursor','selective_and_common_trigram_queries'])
                # Equal sort keys must seek by the final sequence, even deep in
                # an index; exercise both directions and compound grouping.
                for args,first_id in [(['--sort','size','--descending'],50014),
                                      (['--sort','size','--group','folder'],50016)]:
                    seed=run(binary,'results','--session',order_path,*args,'--limit','2','--json')
                    cursor=seed['next_cursor'];cursor['last_sequence']=50000
                    deep=run(binary,'results','--session',order_path,*args,'--limit','3',
                             '--cursor',json.dumps(cursor),'--json')
                    direction=-1 if '--descending' in args else 1
                    check([c['id'] for c in deep['candidates']]==[f'mft-{first_id+direction*i}-7' for i in range(3)],'deep equal-key cursor mismatch')
                checks.append('deep_equal_key_ascending_and_descending_cursors')
                query={'group':'folder','sort':'modified','descending':True}
                client=Client(binary,source,root/'orders',root/'order-output')
                try:
                    client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                    sid=client.tool('open_session',{'path':str(order_path)})['session_id']
                    args={'session_id':sid,'query':query,'request_id':'prepare-order'}
                    job=client.tool('prepare_results',args)
                    check(client.tool('prepare_results',args)==job,'duplicate order worker')
                    done=client.wait(job['job_id']);check(done['state']=='completed' and done['result']['order_index_created'],done)
                    again=client.wait(client.tool('prepare_results',{**args,'request_id':'reuse-order'})['job_id'])
                    check(again['state']=='completed' and not again['result']['order_index_created'],'order cache not reused')
                    values=client.tool('list_candidates',{'session_id':sid,'query':query,'limit':3})
                    check(values['candidates'][0]['id']==candidate['id'],'MCP compound order mismatch')
                    checks.append('mcp_async_compound_prepare_dedup_and_reuse')
                finally:client.close()
                check(file_hash(source)==before,'query preparation modified source')
            checks.append(kind+'_late_selected_cli_mcp_byte_exact')
            print(json.dumps({'case':checks[-1],'status':'passed'}),flush=True)
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
            'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':len(checks),
            'cases':checks,'files_byte_exact':exact,'source_bytes_unchanged':True,
            'scope':'100010 preceding deleted files on each synthetic filesystem; NTFS target past record 2000000. Sparse logical input, no physical disk speed claim.'}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
