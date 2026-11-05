"""Synthetic quick-format remnants with a new MFT; unchanged cluster geometry.

Expected bytes include resident, fragmented, sparse, LZNT1 and ADS streams.
The image changes are controlled; this is not an actual OS quick-format test.
"""
import argparse, hashlib, json, struct, subprocess, tempfile
from pathlib import Path
from e2e import Client, run, check
from e2e_streams import extension_fixture
from make_fixture import record, filename, resident, nonresident, MFT_LCN, SECTOR, RECORD, put

def fixture(path,broken=False):
    truth,_=extension_fixture(path);raw=bytearray(path.read_bytes());new_lcn=1200
    old_mft=MFT_LCN*SECTOR;new_mft=new_lcn*SECTOR
    raw[new_mft:new_mft+64*RECORD]=bytes(64*RECORD)
    raw[new_mft:new_mft+RECORD]=record(0,[nonresident([(128,new_lcn)],64*RECORD)],flags=1,sequence=1)
    raw[new_mft+5*RECORD:new_mft+6*RECORD]=record(5,[filename('.')],flags=3,sequence=1)
    raw[new_mft+50*RECORD:new_mft+51*RECORD]=record(50,[filename('current-live.txt'),resident(0x80,b'current live data')],flags=1)
    put(raw,48,'Q',new_lcn);raw[-SECTOR:]=raw[:SECTOR]
    # An active bit in a detached record does not make it a current live file.
    put(raw,old_mft+20*RECORD+22,'H',1)
    if broken:raw[new_mft:new_mft+RECORD]=bytes(RECORD)
    path.write_bytes(raw)
    expected={f'remnant-{old_mft+int(key.split("-")[1])*RECORD:x}-{key}':value for key,value in truth.items()}
    return expected,new_mft

def main(binary,output=None):
    count=0;files=0
    with tempfile.TemporaryDirectory(prefix='rescue-remnants-') as tmp:
        root=Path(tmp)
        for broken in [False,True]:
            source=root/f'old-{broken}.raw';truth,new=fixture(source,broken);before=hashlib.sha256(source.read_bytes()).hexdigest()
            if not broken:
                quick=run(binary,'scan','--source',source,'--work-dir',root/'quick','--json');check(quick['candidates']==0,'old or current live files entered normal scan');count+=1
            s=run(binary,'scan','--source',source,'--work-dir',root/f'work-{broken}','--fragments','--json')
            rows=run(binary,'results','--session',s['session'],'--limit',100,'--json')['candidates']
            # The former filesystem's live file is also detached after the
            # controlled reformat. Its surviving flag is reported as uncertain.
            detached_live=f'remnant-{MFT_LCN*SECTOR+26*RECORD:x}-mft-26-7'
            check({r['id'] for r in rows}==set(truth)|{detached_live}|({f'remnant-{new+50*RECORD:x}-mft-50-7'} if broken else set()),'remnant enumeration differs '+str(rows))
            check(any('in-use flag' in w for r in rows if r['id']==detached_live for w in r['warnings']),'old live record uncertainty missing')
            selected=[r for r in rows if r['id'] in truth]
            check(all(r['supported'] and r['fragment_offset'] is not None and not r['path_verified'] and r['extent_evidence']=='metadata_remnant' and any('geometry' in w for w in r['warnings']) for r in selected),'remnant uncertainty missing')
            restored=run(binary,'recover','--session',s['session'],*[v for r in selected for v in ['--id',r['id']]],'--destination',root/f'out-{broken}','--json')['results']
            check(len(restored)==len(truth),'missing remnant recovery')
            for r in restored:check(r['status']=='completed' and Path(r['path']).read_bytes().hex()==truth[r['candidate_id']]['hex'] and r['extent_evidence']=='metadata_remnant','remnant bytes or evidence')
            count+=1;files+=len(restored)
            client=Client(binary,source,root/f'mcp-work-{broken}',root/f'mcp-out-{broken}')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                job=client.wait(client.tool('start_scan',{'source':str(source),'fragments':True,'request_id':'scan'})['job_id']);check(job['state']=='completed','MCP fragment scan')
                job=client.wait(client.tool('recover_files',{'session_id':job['result']['session_id'],'ids':list(truth),'destination':str(root/f'mcp-out-{broken}'),'request_id':'recover'})['job_id'])
                check(job['state']=='completed' and len(job['result']['results'])==len(truth),'MCP fragment restore')
                for r in job['result']['results']:check(Path(r['path']).read_bytes().hex()==truth[r['candidate_id']]['hex'],'MCP remnant bytes')
            finally:client.close()
            files+=len(truth);count+=1
            # No directory ancestry was established from the detached record.
            r=run(binary,'recover','--session',s['session'],'--id',selected[0]['id'],'--original','--volume-root',root/f'original-{broken}','--json',expected=5)
            check(r['results'][0]['path'] is None,'invented original path');count+=1
            check(hashlib.sha256(source.read_bytes()).hexdigest()==before,'remnant scan/recovery modified source')
            changed=json.loads(Path(s['session']).read_text());target=next(c for c in changed['candidates'] if c['id']==selected[0]['id']);target['fragment_offset']+=512
            edited=root/f'edited-{broken}.json';edited.write_text(json.dumps(changed))
            r=run(binary,'recover','--session',edited,'--id',selected[0]['id'],'--stream','--destination',root/f'tamper-out-{broken}','--json',expected=5)
            check(r['results'][0]['path'] is None,'tampered record offset used');count+=1
        source=root/'bad-fixup.raw';truth,_=fixture(source);raw=bytearray(source.read_bytes());raw[MFT_LCN*SECTOR+20*RECORD+510]^=1;source.write_bytes(raw)
        s=run(binary,'scan','--source',source,'--work-dir',root/'bad-work','--fragments','--json');rows=run(binary,'results','--session',s['session'],'--json')['candidates']
        check(not any(r['record']==20 for r in rows),'torn remnant accepted');count+=1
        run(binary,'scan','--source',source,'--work-dir',root/'bad-mode','--mode','carve','--fragments','--json',expected=2);count+=1
        # Allocated tail clusters beyond the current MFT's logical size do not
        # establish current record ownership after a quick-format.
        source=root/'allocation-tail.raw';truth,new=fixture(source);raw=bytearray(source.read_bytes())
        raw[new:new+RECORD]=record(0,[nonresident([(128,1200)],16*RECORD)],flags=1,sequence=1);source.write_bytes(raw)
        s=run(binary,'scan','--source',source,'--work-dir',root/'tail-work','--fragments','--json')
        rows=run(binary,'results','--session',s['session'],'--limit',100,'--json')['candidates']
        check(any(r['record']==50 and r['fragment_offset']==new+50*RECORD for r in rows),'allocation tail incorrectly treated as current MFT');count+=1
    result={'status':'passed','checks':count,'files_byte_exact':files,'version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'source_unchanged':True,'scope':__doc__.strip()}
    if output:output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
