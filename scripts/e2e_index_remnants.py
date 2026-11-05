"""Durable detached-MFT scanning with stream boundaries and process loss."""
import argparse,hashlib,json,os,signal,sqlite3,struct,subprocess,tempfile,time
from pathlib import Path
import e2e_ntfs_remnants
from e2e import Client,check,run
from e2e_index_fat import pages
from make_fixture import record,filename,resident


def large(source):
    truth,_=e2e_ntfs_remnants.fixture(source)
    with source.open('r+b') as f:
        f.truncate(64*1024*1024);f.seek(0);boot=bytearray(f.read(512));struct.pack_into('<Q',boot,40,64*1024*1024//512-1)
        f.seek(0);f.write(boot);f.seek(64*1024*1024-512);f.write(boot)
        # Each record has two independent streams. The first crosses a 4 MiB
        # read window; a 128-candidate commit can occur at the next record's start.
        for i in range(1024):
            at=4*1024*1024-512+i*1024;payload=f'old stream {i}'.encode()
            f.seek(at);f.write(record(1000+i,[filename(f'old-{i}.txt'),resident(0x80,payload),resident(0x80,payload+b' ADS',name='notes')]))
    return truth


def main(binary,output=None):
    checks=[];compared=0;exact=0;old_run=e2e_ntfs_remnants.run
    def compare(binary,*args,**kwargs):
        nonlocal compared
        old=old_run(binary,*args,**kwargs)
        if args[0]=='scan' and '--fragments' in args and 'session' in old:
            indexed_args=list(args);indexed_args.insert(1,'--indexed');at=indexed_args.index('--work-dir')+1;indexed_args[at]=str(indexed_args[at])+'-indexed'
            indexed=old_run(binary,*indexed_args,**kwargs)
            check(pages(binary,indexed['session'])==json.loads(Path(old['session']).read_text(encoding='utf-8'))['candidates'],'indexed remnant corpus differs');compared+=1
        return old
    e2e_ntfs_remnants.run=compare
    try:e2e_ntfs_remnants.main(binary)
    finally:e2e_ntfs_remnants.run=old_run
    checks.append('existing_remnant_adversarial_corpus')
    with tempfile.TemporaryDirectory(prefix='rescue-index-remnants-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth=large(source);before=hashlib.sha256(source.read_bytes()).hexdigest()
        old=run(binary,'scan','--source',source,'--work-dir',root/'old','--fragments','--json');expected=json.loads(Path(old['session']).read_text(encoding='utf-8'))
        command=[str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/'crash'),'--fragments','--progress','--json']
        process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=process.stderr.readline();check(bool(line),('ended before stream checkpoint',process.poll()))
            event=json.loads(line);path=Path(event['index'])
            if event['committed_candidates']>=128:break
        process.kill();process.communicate(timeout=20)
        db=sqlite3.connect(path);checkpoint=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);header=json.loads(db.execute('SELECT session FROM checkpoint').fetchone()[0]);saved=checkpoint['remnants']
        check(saved and saved['probe_bytes']==saved['attempts']*1024 and saved['base_records']+saved['attempts']==header['scanned_records'],'probe counter/checkpoint mismatch')
        edited=root/'edited.sqlite';copy=sqlite3.connect(edited);db.backup(copy);bad=json.loads(json.dumps(checkpoint));bad['remnants']['next']+=1;copy.execute('UPDATE checkpoint SET engine_state=?',(json.dumps(bad),));copy.commit();copy.close();db.close()
        invalid=subprocess.run([str(binary),'resume','--session',str(edited),'--json'],capture_output=True,text=True,encoding='utf-8');check(invalid.returncode!=0 and 'invalid MFT remnant checkpoint' in invalid.stdout+invalid.stderr,'unaligned remnant cursor accepted');checks.append('edited_remnant_cursor_rejected')
        resumed=run(binary,'resume','--session',path,'--json');actual=pages(binary,path)
        check(actual==expected['candidates'],'crash resume stream evidence differs');check(resumed['index']['scanned_records']==expected['scanned_records'],'record attempt counted twice after resume')
        checks.extend(['window_crossing_record_and_ads','atomic_record_stream_checkpoint','resume_keeps_probe_budget_and_exact_metadata'])
        args=['recover','--session',path,'--archive',root/'files.zip','--json']
        for candidate_id in truth:args+=['--id',candidate_id]
        run(binary,*args)
        import zipfile
        with zipfile.ZipFile(root/'files.zip') as z:
            check(z.testzip() is None,'ZIP CRC')
            for row in json.loads(z.read('rescue-report.json'))['results']:check(row['status']=='completed' and z.read(row['archive_entry']).hex()==truth[row['candidate_id']]['hex'],row);exact+=1
        checks.append('resident_fragmented_sparse_compressed_ads_bytes')
        limited=run(binary,'scan','--indexed','--source',source,'--work-dir',root/'limit','--fragments','--max-candidates','1','--json',expected=5)
        check(limited['index']['candidates']==1 and limited['index']['state']=='partial' and not limited['index']['resumable'],'candidate limit not terminal');checks.append('terminal_candidate_budget')
        client=Client(binary,source,root/'mcp',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'});job=client.tool('start_scan',{'source':str(source),'indexed':True,'fragments':True,'request_id':'scan'})
            deadline=time.monotonic()+30
            while time.monotonic()<deadline:
                state=client.tool('get_job',{'job_id':job['job_id']})
                if state.get('details') and state['details'].get('pause_supported'):break
                time.sleep(.005)
            client.tool('pause_job',{'job_id':job['job_id']})
            while time.monotonic()<deadline:
                state=client.tool('get_job',{'job_id':job['job_id']})
                if state['state'] not in ['running','pausing','cancelling']:break
                time.sleep(.005)
            check(state['state']=='paused',state);sid=state['result']['session_id'];done=client.wait(client.tool('resume_job',{'session_id':sid,'request_id':'resume'})['job_id']);check(done['state']=='completed',done)
            check(pages(binary,done['result']['session_file'])==expected['candidates'],'MCP remnant checkpoint differs');checks.append('mcp_remnant_pause_resume')
        finally:client.close()
        check(hashlib.sha256(source.read_bytes()).hexdigest()==before,'source modified')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':len(checks),'cases':checks,'adversarial_metadata_comparisons':compared,'files_byte_exact':exact,'source_unchanged':True}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
