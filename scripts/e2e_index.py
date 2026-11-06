"""Real CLI crash/resume, stable cursor, byte recovery and legacy-copy checks."""
import argparse, hashlib, json, os, signal, sqlite3, subprocess, tempfile, time
from pathlib import Path
from e2e import check, run, Client, candidate_evidence
from make_fixture import build, record, resident, filename, MFT_LCN, SECTOR, RECORD


def main(binary, output=None):
    checks=[]
    with tempfile.TemporaryDirectory(prefix='rescue-index-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth=build(source,records=100_000)
        # The large MFT leaves many records after the first durable checkpoint.
        # Payloads are resident so extending the MFT cannot overlap their clusters.
        with source.open('r+b') as f:
            for number in [21,22]:
                payload=bytes.fromhex(truth[f'mft-{number}-7']['hex'])[:80]
                truth[f'mft-{number}-7'].update(hex=payload.hex(),size=len(payload),sha256=hashlib.sha256(payload).hexdigest())
                f.seek(MFT_LCN*SECTOR+number*RECORD)
                f.write(record(number,[filename(truth[f'mft-{number}-7']['name']),resident(0x80,payload)]))
            for number in [150,151,152]:
                f.seek(MFT_LCN*SECTOR+number*RECORD);f.write(bytes(RECORD))
            for number in range(1000,1032):
                data=f'resident {number}'.encode();name=f'late-{number}.txt'
                f.seek(MFT_LCN*SECTOR+number*RECORD);f.write(record(number,[filename(name),resident(0x80,data)]))
                truth[f'mft-{number}-7']={'name':name,'hex':data.hex(),'size':len(data),'sha256':hashlib.sha256(data).hexdigest()}
        original=hashlib.sha256(source.read_bytes()).hexdigest()
        legacy=run(binary,'scan','--source',source,'--work-dir',root/'legacy','--json')
        legacy_path=Path(legacy['session']);legacy_bytes=legacy_path.read_bytes()
        expected=json.loads(legacy_bytes)['candidates'];check(len(expected)==len(truth),'fixture candidates')
        command=[str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/'crash'),'--progress','--json']
        process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=process.stderr.readline();check(bool(line),'scan exited before a checkpoint')
            event=json.loads(line);path=Path(event['index'])
            if event['progress']['scanned_records']>=256:break
        # A live writer must reject a second process while readers remain usable.
        failed=subprocess.run([str(binary),'resume','--session',str(path),'--json'],capture_output=True,text=True,encoding='utf-8')
        check(failed.returncode!=0 and 'writing owner' in failed.stdout+failed.stderr,'concurrent writer accepted')
        checks.append('single_owner_across_processes')
        page=run(binary,'results','--session',path,'--sort','name','--limit','1','--json');cursor=page['next_cursor'];check(cursor,'missing cursor')
        process.kill();process.communicate(timeout=20)
        db=sqlite3.connect(path);saved=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);watermark=db.execute('SELECT watermark FROM checkpoint').fetchone()[0];count=db.execute('SELECT count(*) FROM candidate').fetchone()[0];db.close()
        check(saved['record']>=256 and count==watermark and watermark>=6,'torn checkpoint or candidate batch')
        checks.append('forced_exit_atomic_checkpoint')
        resumed=run(binary,'resume','--session',path,'--json');check(resumed['index']['state']=='completed','resume state')
        new=candidate_evidence(run(binary,'results','--session',path,'--limit','1000','--json'));check(new==expected,'resume metadata differs from independent legacy path')
        checks.append('resume_exact_candidates_no_duplicates')
        old_snapshot=run(binary,'results','--session',path,'--sort','name','--limit','1000','--cursor',json.dumps(cursor),'--json')
        check(old_snapshot['watermark']==page['watermark'] and len(old_snapshot['candidates'])==page['watermark']-1,'snapshot changed after later commit')
        checks.append('live_cursor_stable_after_resume')
        archive=root/'files.zip';run(binary,'recover','--session',path,'--all','--archive',archive,'--json')
        import zipfile
        with zipfile.ZipFile(archive) as z:
            check(z.testzip() is None,'ZIP CRC');rows=json.loads(z.read('rescue-report.json'))['results']
            check(len(rows)==len(truth),'recovery count')
            for row in rows:check(row['status']=='completed' and z.read(row['archive_entry'])==bytes.fromhex(truth[row['candidate_id']]['hex']),'incorrect indexed payload')
        checks.append('indexed_recovery_byte_exact')
        imported=run(binary,'index-import','--session',legacy_path,'--work-dir',root/'import','--json')
        check(legacy_path.read_bytes()==legacy_bytes,'migration overwrote original')
        check(candidate_evidence(run(binary,'results','--session',imported['session'],'--limit','1000','--json'))==expected,'migration candidate mismatch')
        checks.append('legacy_copy_migration')
        client=Client(binary,source,root/'mcp-work',root/'mcp-output')
        try:
            client.rpc('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'index-test','version':'1'}})
            args={'source':str(source),'indexed':True,'request_id':'index-scan'}
            started=client.tool('start_scan',args)
            check(client.tool('start_scan',args)==started,'duplicate indexed job')
            deadline=time.monotonic()+20
            while time.monotonic()<deadline:
                status=client.tool('get_job',{'job_id':started['job_id']})
                if status['details'].get('pause_supported') if isinstance(status.get('details'),dict) else False:break
                time.sleep(.005)
            client.tool('pause_job',{'job_id':started['job_id']})
            while time.monotonic()<deadline:
                status=client.tool('get_job',{'job_id':started['job_id']})
                if status['state'] not in ['running','pausing','cancelling']:break
                time.sleep(.005)
            check(status['state']=='paused' and status['result']['index']['resumable'],('indexed pause',status))
            session=status['result']['session_id'];index_path=status['result']['session_file']
            resumed=client.tool('resume_job',{'session_id':session,'request_id':'resume'})
            check(resumed['job_id']!=started['job_id'],'resuming mutated original job')
            done=client.wait(resumed['job_id']);check(done['state']=='completed',done)
            check(client.tool('get_job',{'job_id':started['job_id']})['state']=='paused','job history overwritten')
            candidates=candidate_evidence(client.tool('list_candidates',{'session_id':session,'limit':1000}));check(candidates==expected,'MCP index candidates differ')
            recovered=client.tool('recover_files',{'session_id':session,'ids':[c['id'] for c in candidates],'destination':str(root/'mcp-output'/'files.zip'),'output_format':'zip','request_id':'recover'})
            check(client.wait(recovered['job_id'])['state']=='completed','MCP indexed recovery')
            with zipfile.ZipFile(root/'mcp-output'/'files.zip') as z:
                for row in json.loads(z.read('rescue-report.json'))['results']:check(z.read(row['archive_entry'])==bytes.fromhex(truth[row['candidate_id']]['hex']),'MCP indexed bytes')
            client.tool('open_session',{'path':str(path)},error=True)
            checks.extend(['mcp_pause_resume_history_and_dedup','mcp_indexed_recovery_byte_exact','mcp_index_work_confinement'])
        finally:client.close()
        client=Client(binary,source,root/'mcp-work',root/'mcp-output')
        try:
            client.rpc('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'reopen-test','version':'1'}})
            opened=client.tool('open_session',{'path':index_path});check(opened['session_id']==session,'session reopen ID')
            check(candidate_evidence(client.tool('list_candidates',{'session_id':session,'limit':1000}))==expected,'reopened MCP index')
            checks.append('mcp_restart_reopen_index')
        finally:client.close()
        # Repeat cancellation cooperatively on Unix. Windows terminate is already
        # tested above as an abrupt process loss; console SIGINT is a native test.
        if os.name!='nt':
            process=subprocess.Popen([str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/'cancel'),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
            first=json.loads(process.stderr.readline());process.send_signal(signal.SIGINT);stdout,stderr=process.communicate(timeout=20)
            check(process.returncode==130,'cancel exit');cancelled=json.loads(stdout);check(cancelled['index']['state']=='cancelled' and cancelled['index']['resumable'],'cancel checkpoint')
            changed=root/'changed.raw';changed.write_bytes(source.read_bytes())
            # Mutate the real source after saving a checkpoint: automatic resume fails.
            with source.open('r+b') as f:f.seek(80);f.write(b'changed!')
            result=subprocess.run([str(binary),'resume','--session',cancelled['session'],'--json'],capture_output=True,text=True,encoding='utf-8')
            check(result.returncode!=0 and 'SOURCE_CHANGED' in result.stdout+result.stderr,('changed source resumed',result.returncode,result.stdout,result.stderr))
            source.write_bytes(changed.read_bytes());checks.extend(['cooperative_cancel_checkpoint','source_change_rejected'])
        check(hashlib.sha256(source.read_bytes()).hexdigest()==original,'scan/recovery modified source bytes')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(Path(binary).read_bytes()).hexdigest(),'checks':len(checks),'cases':checks,'files_byte_exact':len(truth),'source_bytes_unchanged':True}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
