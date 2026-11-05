"""Linux process-memory / end-to-end CLI latency benchmark on an owned image.

Does not manufacture a throughput or GUI frame-time result. An optional GUI PID
is included in the sampled aggregate RSS. Output includes failed/slow queries.
"""
import argparse, hashlib, json, math, os, shutil, signal, statistics, subprocess, threading, time
from pathlib import Path


def rss(pid):
    try:
        lines=(Path('/proc')/str(pid)/'status').read_text().splitlines()
        return next(int(line.split()[1])*1024 for line in lines if line.startswith('VmRSS:'))
    except (OSError,StopIteration):return 0


def main(a):
    if a.repetitions < 1:raise ValueError('--repetitions must be positive')
    if any(rss(pid)==0 for pid in a.gui_pid):raise ValueError('GUI PID is not running at benchmark start')
    a.work.mkdir(exist_ok=True,parents=True)
    initial=a.source.stat();state={'index':None,'candidates':0};peaks=[];jobs=[]
    oracle=json.loads(a.source.with_suffix('.truth.json').read_text())
    def job(arguments,cancel_at=None):
        errors=[];started=time.monotonic();stopped=None;peak=0;own_peak=0;reported=-1
        process=subprocess.Popen([str(a.binary),*map(str,arguments),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        def progress():
            for line in process.stderr:
                try:
                    value=json.loads(line)
                    if 'index' in value:
                        state['index']=value['index'];state['candidates']=value['committed_candidates']
                    else:errors.append(value)
                except ValueError:errors.append(line[:500])
                del errors[:-10]
        reader=threading.Thread(target=progress,daemon=True);reader.start()
        while process.poll() is None:
            own=rss(process.pid);own_peak=max(own_peak,own)
            peak=max(peak,own+sum(rss(pid) for pid in a.gui_pid))
            count=state['candidates'];block=count//250_000
            if block>reported:
                print(json.dumps({'candidates':count,'elapsed_seconds':round(time.monotonic()-started,2),'aggregate_rss_bytes':peak}),flush=True);reported=block
            if stopped is None and ((cancel_at and count>=cancel_at) or shutil.disk_usage(a.work).free<2*1024**3):
                stopped=time.monotonic();process.send_signal(signal.SIGINT)
            time.sleep(.025)
        output=process.stdout.read();reader.join(timeout=5);elapsed=time.monotonic()-started
        cancelled=None if stopped is None else time.monotonic()-stopped
        result=json.loads(output) if output.strip() else None
        record={'exit':process.returncode,'elapsed_seconds':elapsed,'cancel_to_process_exit_seconds':cancelled,
                'aggregate_peak_rss_bytes':peak,'cli_peak_rss_bytes':own_peak,'errors':errors,'result':result}
        jobs.append(record);peaks.append(peak)
        (a.work/'scan-progress.json').write_text(json.dumps({'jobs':jobs,'state':state},indent=2)+'\n')
        return record
    if a.resume_index:
        state['index']=str(a.resume_index)
    else:
        cancelled=job(['scan','--indexed','--source',a.source,'--work-dir',a.work],cancel_at=25_000)
        if cancelled['exit']!=130 or not cancelled['result']['index']['resumable']:
            raise RuntimeError('cooperative cancellation did not leave a resumable checkpoint')
    if a.queries_only:
        if not a.resume_index:raise ValueError('--queries-only requires --resume-index')
        report=json.loads(subprocess.check_output([str(a.binary),'report','--session',state['index'],'--json']))
        if report['index']['candidates']!=oracle['candidates']:raise RuntimeError('reused index count mismatch')
    else:
        done=job(['resume','--session',state['index']])
        if done['exit']!=0 or done['result']['index']['candidates']!=oracle['candidates']:
            raise RuntimeError('large scan incomplete: see scan-progress.json')
    cases={
        'name_asc':['--sort','name'],'name_desc':['--sort','name','--descending'],
        'size_desc':['--sort','size','--descending'],'modified_desc':['--sort','modified','--descending'],
        'group_type_name':['--sort','name','--group','type'],
        'group_folder_size':['--sort','size','--group','folder'],
        'rare_substring':['--sort','name','--name','09999999'],
        'common_substring':['--sort','name','--name','file-'],
        'unicode_absent':['--sort','name','--name','日本語がない'],
    }
    for name,args in [('size_desc_deep',['--sort','size','--descending']),('group_folder_size_deep',['--sort','size','--group','folder'])]:
        seed=json.loads(subprocess.check_output([str(a.binary),'results','--session',state['index'],'--limit','1',*args,'--json']))
        cursor=seed['next_cursor'];cursor['last_sequence']=oracle['candidates']//2
        cases[name]=[*args,'--cursor',json.dumps(cursor,separators=(',',':'))]
    queries={}
    for name,args in cases.items():
        samples=[];failures=[]
        for _ in range(a.repetitions):
            start=time.monotonic()
            p=subprocess.Popen([str(a.binary),'results','--session',state['index'],'--limit','500',*args,'--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            output=[]
            def collect():output.append(p.communicate())
            reader=threading.Thread(target=collect);reader.start();peak=0
            while reader.is_alive():
                peak=max(peak,rss(p.pid)+sum(rss(pid) for pid in a.gui_pid));time.sleep(.005)
            reader.join();peaks.append(peak);samples.append((time.monotonic()-start)*1000)
            if p.returncode:failures.append({'exit':p.returncode,'error':output[0][1].decode('utf-8','replace')[:1000]})
            else:
                rows=json.loads(output[0][0])['candidates']
                count=0 if name=='unicode_absent' else (1 if name=='rare_substring' else 500)
                descending=name in ['name_desc','size_desc','modified_desc','size_desc_deep']
                first=(oracle['candidates']+15) if descending or name=='rare_substring' else 16
                if name.endswith('_deep'):first=oracle['candidates']//2+(14 if descending else 16)
                expected=[f'mft-{first+(-i if descending else i)}-7' for i in range(count)]
                if [r['id'] for r in rows]!=expected:failures.append({'error':'ordered candidate IDs differ from the deterministic oracle'})

        ordered=sorted(samples);queries[name]={'samples_ms':samples,'first_query_ms':samples[0],'p95_ms':ordered[math.ceil(.95*len(ordered))-1],'median_ms':statistics.median(samples),'failures':failures}
        print(json.dumps({'query':name,**queries[name]}),flush=True)
    recovered=subprocess.run([str(a.binary),'recover','--session',state['index'],
        *[value for key in oracle['examples'] for value in ['--id',key]],'--destination',str(a.work/'selected'),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=90)
    if recovered.returncode:raise RuntimeError(recovered.stderr)
    results=json.loads(recovered.stdout)['results']
    for row in results:
        if row['status']!='completed' or Path(row['path']).read_bytes()!=bytes.fromhex(oracle['examples'][row['candidate_id']]['hex']):
            raise RuntimeError('large-index selected bytes mismatch')
    final=a.source.stat()
    passed=all(not query['failures'] for query in queries.values())
    report={'status':'passed' if passed else 'failed','version':subprocess.check_output([str(a.binary),'--version'],text=True).strip(),
        'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'logical_source_bytes':initial.st_size,
        'allocated_source_bytes':initial.st_blocks*512,'candidates':oracle['candidates'],'index':state['index'],
        'index_bytes':Path(state['index']).stat().st_size,'jobs':jobs,'aggregate_peak_rss_bytes':max(peaks),
        'gui_pids_included':a.gui_pid,'queries_only':a.queries_only,'queries':queries,'selected_files_byte_exact':len(results),
        'source_size_and_mtime_unchanged':(initial.st_size,initial.st_mtime_ns)==(final.st_size,final.st_mtime_ns),
        'scope':'Sparse synthetic NTFS, resident data, Linux. RSS sampled every 25 ms (5 ms for query processes); CLI latency includes process startup and JSON. GUI frame/keyboard latency and physical 1 TiB throughput are not measured. Source identity and metadata revalidated; no full 1 TiB hash claim.'}
    a.output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps({'status':report['status'],'report':str(a.output)}))
    if not passed:raise SystemExit(1)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--source',type=Path,required=True);p.add_argument('--work',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--resume-index',type=Path);p.add_argument('--queries-only',action='store_true');p.add_argument('--gui-pid',type=int,action='append',default=[]);p.add_argument('--repetitions',type=int,default=20);main(p.parse_args())
