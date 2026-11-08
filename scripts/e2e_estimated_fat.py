"""Explicit FAT contiguous estimates: byte truth, refusal paths and uncertainty.

A structurally unknown fragmented BIN intentionally produces an incorrect guess;
that output must remain labelled estimated and is never counted byte-exact.
"""
import argparse,hashlib,json,struct,subprocess,tempfile
from pathlib import Path
from e2e import Client,check,run,candidate_evidence
from e2e_index_fat import pages as indexed_pages
from e2e_classic_fat import make
from make_fat_fixture import fat32,file_hash,put


def pages(binary,session):
    if str(session).endswith(".json"):
        return json.loads(Path(session).read_text(encoding="utf-8"))["candidates"]
    return indexed_pages(binary,session)


def write(path,at,data):
    with path.open('r+b') as f:f.seek(at);f.write(data)


def main(binary,output=None):
    checks=[];exact=0;mismatches=0
    with tempfile.TemporaryDirectory(prefix='rescue-fat-estimate-') as temporary:
        root=Path(temporary)
        for bits in [12,16,32]:
            source=root/f'fat{bits}.raw'
            if bits==32:
                truth,unsupported,meta=fat32(source);key=f"fat32-{meta['positions']['LOST    BIN']}"
            else:truth,unsupported,meta=make(source,bits);key=meta['lost']
            data=b'explicit estimated file\n'*39+b'end\n';data=data[:900].ljust(900,b'!')
            address=lambda c:meta['heap']+(c-2)*512
            write(source,address(110),data);before=file_hash(source)
            old=run(binary,'scan','--source',source,'--work-dir',root/f'{bits}-off','--json')
            check(not next(c for c in pages(binary,old['session']) if c['id']==key)['supported'],'estimation enabled by default')
            estimates=[]
            for indexed in [False,True]:
                args=['scan','--source',source,'--work-dir',root/f'{bits}-on-{indexed}','--estimate-fat-contiguous','--json']
                if indexed:args.append('--indexed')
                scan=run(binary,*args);candidate=next(c for c in pages(binary,scan['session']) if c['id']==key)
                check(candidate['supported'] and candidate['extent_evidence']=='estimated_contiguous' and candidate['allocation_status']=='unallocated',candidate)
                check(any('ESTIMATED_CONTIGUOUS:' in w and 'unverified' in w for w in candidate['warnings']),'missing uncertainty')
                estimates.append(candidate)
                recovered=run(binary,'recover','--session',scan['session'],'--id',key,'--destination',root/f'{bits}-out-{indexed}','--json')['results'][0]
                check(recovered['status']=='completed' and recovered['extent_evidence']=='estimated_contiguous' and Path(recovered['path']).read_bytes()==data,recovered);exact+=1
                check(any('ESTIMATED_CONTIGUOUS:' in w for w in recovered['warnings']),'report lost estimated evidence')
            check(estimates[0]==estimates[1],'legacy/index estimates differ')
            client=Client(binary,source,root/f'{bits}-mcp-work',root/f'{bits}-mcp-out')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                job=client.wait(client.tool('start_scan',{'source':str(source),'indexed':True,'estimate_fat_contiguous':True,'request_id':'estimate'})['job_id'])
                restored=client.wait(client.tool('recover_files',{'session_id':job['result']['session_id'],'ids':[key],'destination':str(root/f'{bits}-mcp-out'),'request_id':'recover-estimate'})['job_id'])
                row=restored['result']['results'][0]
                check(row['extent_evidence']=='estimated_contiguous' and Path(row['path']).read_bytes()==data,row);exact+=1
            finally:client.close()
            check(file_hash(source)==before,'estimated source modified')
            checks += [f'fat{bits}_explicit_opt_in_only',f'fat{bits}_legacy_index_mcp_bytes_and_evidence',f'fat{bits}_reloaded_restore_preserves_policy']
        # Kill an actual in-progress ownership traversal; policy survives the checkpoint.
        from e2e_index_fat import large
        source=root/'resume.raw';large(source,'fat32',fat32);before=file_hash(source)
        command=[str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/'resume-work'),'--estimate-fat-contiguous','--progress','--json']
        process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=process.stderr.readline();check(bool(line),('early resume fixture exit',process.poll()))
            event=json.loads(line);session=Path(event['index'])
            if event['progress']['scanned_records']>=128:break
        process.kill();process.communicate(timeout=20)
        check(run(binary,'resume','--session',session,'--json')['index']['state']=='completed','estimate resume incomplete')
        resumed=pages(binary,session)
        old=run(binary,'scan','--source',source,'--work-dir',root/'resume-legacy','--estimate-fat-contiguous','--json')
        expected=pages(binary,old['session']);check(resumed==expected,[(left['id'],{k:[left.get(k),right.get(k)] for k in set(left)|set(right) if left.get(k)!=right.get(k)}) for left,right in zip(resumed,expected) if left!=right])
        check(any(c['extent_evidence']=='estimated_contiguous' for c in resumed),'resume lost opt-in')
        check(file_hash(source)==before,'resume changed source');checks.append('crash_resume_preserves_policy_and_evidence')
        # FAT32 fixture offers exact table offsets for adversarial conditions.
        original=root/'adversarial.raw';_,_,meta=fat32(original)
        slot=meta['positions']['LOST    BIN'];key=f'fat32-{slot}';address=lambda c:meta['heap']+(c-2)*512
        jpg=(Path(__file__).resolve().parent.parent/'tests/corpus/files/sample.jpg').read_bytes()
        write(original,slot+8,b'JPG');write(original,slot+20,struct.pack('<H',0));write(original,slot+26,struct.pack('<H',300));write(original,slot+28,struct.pack('<I',len(jpg)));write(original,address(300),jpg)
        for name,modify,accepted in [
            ('valid_jpeg',lambda p:None,True),
            ('broken_jpeg',lambda p:write(p,address(300)+len(jpg)-2,b'\0\0'),False),
            ('fragmented_jpeg',lambda p:(write(p,address(302),jpg[512:]),write(p,address(301),b'\0'*512)),False),
            ('allocated_next',lambda p:[write(p,meta['fat']+copy*meta['fat_sectors']*512+301*4,struct.pack('<I',0x0fffffff)) for copy in range(2)],False),
            ('mirror_disagreement',lambda p:write(p,meta['fat']+meta['fat_sectors']*512+301*4,struct.pack('<I',0x0fffffff)),False),
            ('ownership_incomplete',lambda p:[write(p,meta['fat']+copy*meta['fat_sectors']*512+500*4,bytes(4)) for copy in range(2)],False),
            ('outside_heap',lambda p:write(p,slot+26,struct.pack('<H',1)),False),
        ]:
            source=root/(name+'.raw');source.write_bytes(original.read_bytes());modify(source);before=file_hash(source)
            scan=run(binary,'scan','--indexed','--source',source,'--work-dir',root/(name+'-work'),'--estimate-fat-contiguous','--json',expected=5 if name=='ownership_incomplete' else 0)
            candidate=next(c for c in pages(binary,scan['session']) if c['id']==key)
            check(candidate['supported']==accepted,(name,candidate))
            if accepted:
                row=run(binary,'recover','--session',scan['session'],'--id',key,'--destination',root/(name+'-out'),'--json')['results'][0]
                check(Path(row['path']).read_bytes()==jpg and row['extent_evidence']=='estimated_contiguous',row);exact+=1
                check(any('Format structure was validated' in w for w in candidate['warnings']),'JPEG validator not invoked')
            else:
                check(candidate['warnings'],'refusal without reason')
                row=run(binary,'recover','--session',scan['session'],'--id',key,'--destination',root/(name+'-out'),'--json',expected=5)['results'][0]
                check(row['status']=='failed' and row['path'] is None,'refused estimate wrote output')
            check(file_hash(source)==before,'adversarial source modified');checks.append(name)
        source=root/'unknown-fragment.raw';source.write_bytes(original.read_bytes());write(source,slot+8,b'BIN');write(source,slot+28,struct.pack('<I',900));write(source,address(300),b'A'*512);write(source,address(301),bytes(512));write(source,address(302),b'B'*388);before=file_hash(source)
        scan=run(binary,'scan','--source',source,'--work-dir',root/'unknown-work','--estimate-fat-contiguous','--json')
        row=run(binary,'recover','--session',scan['session'],'--id',key,'--destination',root/'unknown-out','--json')['results'][0]
        check(Path(row['path']).read_bytes()!=b'A'*512+b'B'*388 and row['extent_evidence']=='estimated_contiguous','unknown format estimate misclassified')
        check(any('No structural validator' in w and 'unverified' in w for w in row['warnings']),'unknown format uncertainty missing')
        mismatches+=1;check(file_hash(source)==before,'unknown source modified');checks.append('unverifiable_fragment_is_estimated_and_not_counted_exact')
        denied=subprocess.run([str(binary),'scan','--source',str(original),'--work-dir',str(root/'invalid-policy'),'--mode','carve','--estimate-fat-contiguous','--json'],capture_output=True)
        check(denied.returncode!=0,'estimate enabled for carving');checks.append('carving_policy_refused')
    report={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'known_incorrect_estimates_labelled_unverified':mismatches,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(report,ensure_ascii=False));return report


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
