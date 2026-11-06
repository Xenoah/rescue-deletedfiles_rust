"""Durable FAT ownership traversal, crash/restart and real byte recovery."""
import argparse,hashlib,json,os,signal,sqlite3,struct,subprocess,tempfile,time
from pathlib import Path
import e2e_fat
from e2e import Client,check,run, candidate_evidence
from make_fat_fixture import fat32,exfat,exfat_set,sfn,file_hash


def pages(binary,path):
    result=[];cursor=None
    while True:
        args=['results','--session',path,'--limit','1000','--json']
        if cursor:args+=['--cursor',json.dumps(cursor)]
        page=run(binary,*args);result+=candidate_evidence(page);cursor=page['next_cursor']
        if cursor is None:return result


def large(source,kind,maker,count=4096,tail=False):
    truth,unsupported,meta=maker(source);cluster=meta['cluster'];address=lambda c:meta['heap']+(c-2)*cluster
    initial=[2,8] if kind=='fat32' else [2,40]
    with source.open('r+b') as f:
        original=b''
        for c in initial:f.seek(address(c));original+=f.read(cluster)
        slots=[original[i:i+32] for i in range(0,len(original),32)]
        slots=slots[:next(i for i,s in enumerate(slots) if s[0]==0)]
        for i in range(count):
            if kind=='fat32':slots.append(sfn(f'F{i:07d}TXT'.encode(),0,0))
            else:slots.extend(exfat_set(f'empty-{i:04d}.txt',0,0,contiguous=False))
        tail_slot=len(slots)
        if tail:
            payload=b'known first cluster\n' if kind=='fat32' else b'known first exFAT cluster'
            if kind=='fat32':slots.append(sfn(b'LAST    PRF',30,len(payload)))
            else:slots.extend(exfat_set('late.prf',190,len(payload),contiguous=False))
        data=b''.join(slots)+bytes(32);count=(len(data)+cluster-1)//cluster
        chain=initial+list(range(2000,2000+count-2));data=data.ljust(count*cluster,b'\0')
        if tail:
            offset=address(chain[tail_slot*32//cluster])+tail_slot*32%cluster
            truth[f'{kind}-{offset}']=payload
        for copy in range(2 if kind=='fat32' else 1):
            base=meta['fat']+(copy*meta['fat_sectors']*512 if kind=='fat32' else 0)
            for i,c in enumerate(chain):
                f.seek(base+c*4);f.write(struct.pack('<I',chain[i+1] if i+1<len(chain) else (0x0fffffff if kind=='fat32' else 0xffffffff)))
        for i,c in enumerate(chain):f.seek(address(c));f.write(data[i*cluster:(i+1)*cluster])
        if kind=='exfat':
            for c in chain:
                offset=meta['bitmap']+(c-2)//8;f.seek(offset);value=f.read(1)[0];f.seek(offset);f.write(bytes([value|(1<<((c-2)%8))]))
    return truth


def main(binary,output=None):
    checks=[];exact=0;compared=0
    # Reuse the complete adversarial corpus, with two actual scanning paths.
    # Restore tests retain their legacy session so they also check shared parser changes.
    old_scan=e2e_fat.scan
    def compare_scan(binary,source,work,expected=0):
        nonlocal compared
        old=old_scan(binary,source,work,expected)
        if 'session' not in old:return old
        indexed=run(binary,'scan','--indexed','--source',source,'--work-dir',Path(str(work)+'-indexed'),'--json',expected=expected)
        legacy=json.loads(Path(old['session']).read_text(encoding='utf-8'))
        check(pages(binary,indexed['session'])==legacy['candidates'],('FAT index differs',source))
        header=run(binary,'report','--session',indexed['session'],'--json')['session']
        check(header['invalid_records']==old['invalid_records'] and header['truncated']==old['truncated'],('FAT incomplete state differs',source))
        compared+=1;return old
    e2e_fat.scan=compare_scan
    try:e2e_fat.main(binary)
    finally:e2e_fat.scan=old_scan
    checks.append('shared_adversarial_corpus_exact_metadata')
    with tempfile.TemporaryDirectory(prefix='rescue-index-fat-') as temporary:
        root=Path(temporary)
        from e2e_disks import disk_image
        from make_fixture import build
        volumes=[];disk_truth={}
        for i,(kind,maker) in enumerate([('ntfs',build),('fat32',fat32),('exfat',exfat)],1):
            volume=root/('mixed-'+kind+'.raw');made=maker(volume);truth=made if kind=='ntfs' else made[0]
            disk_truth.update({f'p{i}:'+k:bytes.fromhex(v['hex']) if kind=='ntfs' else v for k,v in truth.items()});volumes.append(volume)
        for scheme,sector in [('mbr',512),('gpt',4096)]:
            source=root/f'mixed-{scheme}.raw';disk_image(source,volumes,sector,scheme);before=file_hash(source)
            old=run(binary,'scan','--source',source,'--work-dir',root/(scheme+'-old'),'--json')
            indexed=run(binary,'scan','--indexed','--source',source,'--work-dir',root/(scheme+'-index'),'--json')
            check(pages(binary,indexed['session'])==json.loads(Path(old['session']).read_text(encoding='utf-8'))['candidates'],'mixed volume order/evidence')
            result=run(binary,'recover','--session',indexed['session'],'--all','--destination',root/(scheme+'-out'),'--json')
            for row in result['results']:check(row['status']=='completed' and Path(row['path']).read_bytes()==disk_truth[row['candidate_id']],row);exact+=1
            check(file_hash(source)==before,'mixed disk source modified');checks.append('mixed_'+scheme+'_metadata_and_bytes')
        for kind,maker in [('fat32',fat32),('exfat',exfat)]:
            source=root/(kind+'.raw');truth=large(source,kind,maker);before=file_hash(source)
            old=run(binary,'scan','--source',source,'--work-dir',root/(kind+'-legacy'),'--json')
            expected=json.loads(Path(old['session']).read_text(encoding='utf-8'))['candidates']
            command=[str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/(kind+'-crash')),'--progress','--json']
            process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
            while True:
                line=process.stderr.readline();check(bool(line),('early exit',kind,process.poll()))
                event=json.loads(line);path=Path(event['index'])
                if event['progress']['scanned_records']>=128:break
            process.kill();process.communicate(timeout=20)
            db=sqlite3.connect(path);saved=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);count=db.execute('SELECT count(*) FROM engine_item').fetchone()[0];public=db.execute('SELECT count(*) FROM candidate').fetchone()[0];db.close()
            check(saved['fat']['phase']=='Traverse' and saved['fat']['staged']==count and count>0 and public==0,('unvalidated candidates published or torn stage',kind))
            check(saved['fat']['active'] is not None,'directory cursor not saved')
            # Build the actual earlier table layout from a consistent snapshot;
            # migration must retain both unpublished FAT rows and resumability.
            legacy_path=root/(kind+'-schema1.db');original_db=sqlite3.connect(path);legacy_db=sqlite3.connect(legacy_path);original_db.backup(legacy_db);original_db.close()
            legacy_db.executescript('ALTER TABLE candidate RENAME TO candidate_v2; CREATE TABLE candidate AS SELECT seq,id,payload,discovery_key,name_key,path_key,size_key,created_key,modified_key,search_text,supported FROM candidate_v2; DROP TABLE candidate_v2; PRAGMA user_version=1;');legacy_db.close()
            original_hash=file_hash(legacy_path)
            migrated=run(binary,'index-import','--session',legacy_path,'--work-dir',root/(kind+'-migration'),'--json')
            check(file_hash(legacy_path)==original_hash,'migration changed original SQLite bytes')
            check(migrated['index']['resumable'],'migration lost restart point')
            check(run(binary,'resume','--session',migrated['session'],'--json')['index']['state']=='completed','migrated FAT resume failed')
            check(pages(binary,migrated['session'])==expected,'schema1 copy lost candidate metadata')
            check(file_hash(legacy_path)==original_hash,'resume changed original SQLite bytes')
            checks.append(kind+'_schema1_copy_preserves_staging_and_evidence')
            resumed=run(binary,'resume','--session',path,'--json');check(resumed['index']['state']=='completed',resumed)
            check(pages(binary,path)==expected,('restart metadata mismatch',kind))
            checks.extend([kind+'_atomic_staging_before_publication',kind+'_mid_directory_crash_resume_exact'])
            args=['recover','--session',path,'--archive',root/(kind+'.zip'),'--json']
            for candidate_id in truth:args+=['--id',candidate_id]
            result=run(binary,*args)
            import zipfile
            with zipfile.ZipFile(root/(kind+'.zip')) as z:
                check(z.testzip() is None,'ZIP CRC')
                for row in json.loads(z.read('rescue-report.json'))['results']:
                    check(row['status']=='completed' and z.read(row['archive_entry'])==truth[row['candidate_id']],row);exact+=1
            checks.append(kind+'_selected_byte_exact')
            client=Client(binary,source,root/(kind+'-mcp'),root/(kind+'-mcp-output'))
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                started=client.tool('start_scan',{'source':str(source),'indexed':True,'request_id':'scan'})
                deadline=time.monotonic()+30
                while time.monotonic()<deadline:
                    job=client.tool('get_job',{'job_id':started['job_id']})
                    if job.get('details') and job['details'].get('pause_supported'):break
                    time.sleep(.005)
                client.tool('pause_job',{'job_id':started['job_id']})
                while time.monotonic()<deadline:
                    job=client.tool('get_job',{'job_id':started['job_id']})
                    if job['state'] not in ['running','pausing','cancelling']:break
                    time.sleep(.005)
                check(job['state']=='paused' and job['result']['index']['resumable'],job)
                session=job['result']['session_id'];resumed=client.wait(client.tool('resume_job',{'session_id':session,'request_id':'resume'})['job_id']);check(resumed['state']=='completed',resumed)
                check(pages(binary,resumed['result']['session_file'])==expected,'MCP FAT resume differs')
                checks.append(kind+'_mcp_pause_resume')
            finally:client.close()
            if os.name!='nt':
                command[command.index('--work-dir')+1]=str(root/(kind+'-cancel'))
                process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
                first=json.loads(process.stderr.readline());process.send_signal(signal.SIGINT);stdout,stderr=process.communicate(timeout=20)
                check(process.returncode==130,(stdout,stderr));stopped=json.loads(stdout)
                check(stopped['index']['resumable'] and stopped['index']['state']=='cancelled','cancel not resumable')
                check(run(binary,'resume','--session',stopped['session'],'--json')['index']['state']=='completed','cancel resume incomplete')
                checks.append(kind+'_cooperative_cancel_resume')
            check(file_hash(source)==before,'FAT source modified')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'adversarial_metadata_comparisons':compared,'files_byte_exact':exact,'source_unchanged':True}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
