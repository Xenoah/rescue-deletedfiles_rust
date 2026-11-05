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
