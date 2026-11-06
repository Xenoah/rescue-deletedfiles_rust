"""Independent allocation maps, live/free signatures and real CLI/MCP parity."""
import argparse, json, os, sqlite3, struct, subprocess, tempfile
from pathlib import Path
from e2e import Client, check, run, candidate_evidence
from e2e_disks import disk_image
from make_fixture import build, record, filename, resident, MFT_LCN, RECORD, SECTOR
from make_fat_fixture import fat32, exfat, file_hash
from make_carve_fixture import png


def fixture(path, kind):
    payload=png();check(40<len(payload)<512,'fixture must cross one cluster boundary')
    if kind=='ntfs':
        build(path);meta={'heap':0,'cluster':512};bitmap=bytearray(256)
        # Mark boot and MFT metadata allocated; files are well beyond that area.
        for c in [*range(132),610,621,2047]:bitmap[c//8]|=1<<(c%8)
        with path.open('r+b') as f:
            f.seek(MFT_LCN*SECTOR+6*RECORD)
            f.write(record(6,[filename('$Bitmap'),resident(0x80,bitmap)],flags=1,sequence=1))
        address=lambda c:c*512
    else:
        _,_,meta=(fat32 if kind=='fat32' else exfat)(path)
        address=lambda c:meta['heap']+(c-2)*meta['cluster']
        with path.open('r+b') as f:
            for c in [610,621]:
                if kind=='fat32':
                    for copy in range(2):
                        f.seek(meta['fat']+copy*meta['fat_sectors']*512+c*4);f.write(struct.pack('<I',0x0fffffff))
                else:
                    f.seek(meta['bitmap']+(c-2)//8);value=f.read(1)[0]
                    f.seek(-1,1);f.write(bytes([value|1<<((c-2)%8)]))
    positions={'free':address(600)+7,'live':address(610)+7,
               'cross_allocated':address(620)+meta['cluster']-20,
               'cross_free':address(630)+meta['cluster']-20}
    # This FAT entry sits beyond the source fingerprint prefix; changing it
    # independently exercises allocation revalidation before restore/preview.
    if kind=='fat32':positions['late_free']=address(20000)+7
    with path.open('r+b') as f:
        for at in positions.values():f.seek(at);f.write(payload)
    return meta,positions,payload


def main(binary,output=None):
    cases=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-allocation-') as temporary:
        root=Path(temporary);serial=0;volumes=[];expected_disks=[]
        def scan(source,scope=None,indexed=False):
            nonlocal serial
            serial+=1;args=['scan','--source',source,'--mode','carve','--extension','png',
                           '--work-dir',root/f'work-{serial}','--json']
            if scope is not None:args+=['--carve-scope',scope]
            if indexed:args+=['--indexed']
            found=run(binary,*args,timeout=90)
            rows=candidate_evidence(run(binary,'results','--session',found['session'],'--limit',1000,'--json'))
            header=run(binary,'report','--session',found['session'],'--json')['session'] if indexed else json.loads(Path(found['session']).read_text(encoding='utf-8'))
            return found,rows,header
        def recover(session,rows,payload,label):
            nonlocal exact
            result=run(binary,'recover','--session',session,'--all','--destination',root/label,'--json')
            check(len(result['results'])==len(rows),'selected count differs')
            for row in result['results']:
                check(row['status']=='completed' and Path(row['path']).read_bytes()==payload,row);exact+=1
        for kind in ['ntfs','fat32','exfat']:
            source=root/(kind+'.raw');meta,positions,payload=fixture(source,kind);before=file_hash(source)
            wanted={v for k,v in positions.items() if k not in ['live','cross_allocated']}
            old,rows,header=scan(source)
            check(header['search']['carve_scope']=='unallocated','wrong default')
            check({c['record'] for c in rows}==wanted,(kind,rows))
            check(all(c['allocation_status']=='unallocated' for c in rows),rows)
            check(not any(w.startswith('ALLOCATION_FALLBACK:') for w in header['warnings']),header['warnings'])
            indexed,index_rows,_=scan(source,indexed=True);check(rows==index_rows,'indexed evidence differs')
            recover(indexed['session'],index_rows,payload,kind+'-out')
            all_scan,all_rows,all_header=scan(source,'all')
            check({c['record'] for c in all_rows}==set(positions.values()),'all scope dropped allocated bytes')
            check(all(c['allocation_status']=='unknown' for c in all_rows),'all scope claimed allocation')
            # Serialized settings predating carve_scope mean all bytes, even
            # after the default for new scans has changed to unallocated.
            legacy=json.loads(Path(all_scan['session']).read_text(encoding='utf-8'))
            del legacy['search']['carve_scope'];legacy_path=root/(kind+'-legacy.json')
            legacy_path.write_text(json.dumps(legacy),encoding='utf-8')
            recover(legacy_path,all_rows,payload,kind+'-legacy-out')
            client=Client(binary,source,root/(kind+'-mcp-work'),root/(kind+'-mcp-out'))
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                for scope,expected in [(None,wanted),('all',set(positions.values()))]:
                    args={'source':str(source),'mode':'carve','extensions':['png'],'indexed':True,'request_id':str(scope)}
                    if scope is not None:args['carve_scope']=scope
                    job=client.wait(client.tool('start_scan',args)['job_id']);check(job['state']=='completed',job)
                    actual=candidate_evidence(client.tool('list_candidates',{'session_id':job['result']['session_id']}))
                    check({c['record'] for c in actual}==expected,'MCP scope differs')
                client.tool('start_scan',{'source':str(source),'mode':'carve','carve_scope':'invalid','request_id':'invalid'},error=True)
                client.tool('start_scan',{'source':str(source),'mode':'carve','carve_scope':None,'request_id':'null'},error=True)
            finally:client.close()
            check(file_hash(source)==before,'scope scan modified source')
            cases.extend([kind+'_whole_payload_allocation_and_cross_cluster',kind+'_json_index_mcp_scope_parity',kind+'_legacy_all_scope_recovery'])
            volumes.append(source);expected_disks.append(wanted)
            if kind=='fat32':
                target=next(c for c in rows if c['record']==positions['late_free'])
                entries=[meta['fat']+copy*meta['fat_sectors']*512+20000*4 for copy in range(2)]
                original_stat=source.stat()
                try:
                    with source.open('r+b') as f:
                        for at in entries:f.seek(at);f.write(struct.pack('<I',0x0fffffff))
                    # A raw device does not expose a file modification time.
                    # Preserve this test file's timestamp to simulate that case.
                    os.utime(source,ns=(original_stat.st_atime_ns,original_stat.st_mtime_ns))
                    _,_,changed_header=scan(source,'all')
                    check(changed_header['fingerprint']==header['fingerprint'],'test must change only uncaptured allocation bytes')
                    failure=run(binary,'recover','--session',indexed['session'],'--id',target['id'],
                                '--destination',root/'changed-allocation','--json',expected=5)
                    check(failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(failure),failure)
                    stream_failure=run(binary,'recover','--session',indexed['session'],'--id',target['id'],
                                       '--destination',root/'changed-stream','--stream','--json',expected=5)
                    check(stream_failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(stream_failure),stream_failure)
                    check(not any(p.is_file() and p.suffix!='.json' for p in (root/'changed-stream').rglob('*')),'streaming wrote changed allocation bytes')
                    failure=run(binary,'preview','--session',indexed['session'],'--id',target['id'],
                                '--destination',root/'changed-preview','--json',expected=4)
                    check('SOURCE_CHANGED' in failure,failure)
                    cases.append('allocation_change_outside_fingerprint_rejected_before_restore_stream_and_preview')
                finally:
                    with source.open('r+b') as f:
                        for at in entries:f.seek(at);f.write(bytes(4))
                    os.utime(source,ns=(original_stat.st_atime_ns,original_stat.st_mtime_ns))
                check(file_hash(source)==before,'test mutation not restored')
        for scheme,sector in [('mbr',512),('gpt',4096)]:
            source=root/(scheme+'.raw');starts=disk_image(source,volumes,sector,scheme);before=file_hash(source)
            expected={start*sector+at for (start,_),values in zip(starts,expected_disks) for at in values}
            _,rows,_=scan(source,indexed=True)
            check({c['record'] for c in rows}==expected,(scheme,rows))
            check(all(c['allocation_status']=='unallocated' for c in rows),'partition-relative allocation wrong')
            check(file_hash(source)==before,'disk source changed');cases.append(scheme+'_mixed_volume_allocation_offsets')
        # Unreadable maps retain candidates but must expose a fallback reason.
        for kind in ['ntfs','fat32','exfat','raw']:
            source=root/('broken-'+kind+'.raw')
            if kind=='raw':source.write_bytes(bytes(4096)+png()+bytes(4096));wanted=None
            else:
                meta,positions,_=fixture(source,kind);wanted=set(positions.values())
                with source.open('r+b') as f:
                    if kind=='ntfs':f.seek(MFT_LCN*SECTOR+6*RECORD);f.write(bytes(RECORD))
                    elif kind=='fat32':
                        # Inconsistent root allocation invalidates the map.
                        f.seek(meta['fat']+meta['fat_sectors']*512+2*4);f.write(bytes(4))
                    else:f.seek(meta['heap']);f.write(bytes(32)) # remove bitmap entry
            before=file_hash(source);_,rows,header=scan(source,indexed=True)
            if wanted is not None:check({c['record'] for c in rows}==wanted,(kind,rows))
            else:check(len(rows)==1,rows)
            check(all(c['allocation_status']=='unknown' for c in rows),rows)
            check(any(w.startswith('ALLOCATION_FALLBACK:') for w in header['warnings']),header['warnings'])
            check(file_hash(source)==before,'fallback modified source');cases.append(kind+'_explicit_unknown_allocation_fallback')
        # Actual process loss; scope changes cannot be smuggled into a resume.
        source=root/'resume.raw'
        with source.open('xb') as f:f.write(bytes(4096)+png());f.seek(96*1024*1024);f.write(png());f.truncate(128*1024*1024)
        before=file_hash(source)
        p=subprocess.Popen([str(binary),'scan','--source',str(source),'--indexed','--mode','carve','--work-dir',str(root/'crash'),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=p.stderr.readline();check(bool(line),'scan ended before checkpoint');event=json.loads(line)
            if event['progress']['scanned_records']>=1024*1024:break
        index=Path(event['index']);p.kill();p.communicate(timeout=20)
        db=sqlite3.connect(index);copy_path=root/'forged.sqlite';copy=sqlite3.connect(copy_path);db.backup(copy)
        header=json.loads(copy.execute('SELECT session FROM checkpoint').fetchone()[0]);header['search']['carve_scope']='all'
        copy.execute('UPDATE checkpoint SET session=?',(json.dumps(header),));copy.commit();copy.close()
        legacy_path=root/'legacy-checkpoint.sqlite';legacy=sqlite3.connect(legacy_path);db.backup(legacy);db.close()
        saved=json.loads(legacy.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);saved['version']=1;del saved['scope']
        header=json.loads(legacy.execute('SELECT session FROM checkpoint').fetchone()[0]);del header['search']['carve_scope']
        legacy.execute('UPDATE checkpoint SET session=?,engine_state=?',(json.dumps(header),json.dumps(saved)));legacy.commit();legacy.close()
        invalid=subprocess.run([str(binary),'resume','--session',str(copy_path),'--json'],capture_output=True,text=True,encoding='utf-8')
        check(invalid.returncode!=0 and 'INVALID_REQUEST' in invalid.stderr,'forged allocation scope resumed')
        run(binary,'resume','--session',index,'--json',timeout=90)
        rows=candidate_evidence(run(binary,'results','--session',index,'--json'))
        check({c['record'] for c in rows}=={4096,96*1024*1024},rows)
        recover(index,rows,png(),'resumed-out');check(file_hash(source)==before,'resume changed source')
        run(binary,'resume','--session',legacy_path,'--json',timeout=90)
        legacy_rows=candidate_evidence(run(binary,'results','--session',legacy_path,'--json'))
        check(legacy_rows==rows,'legacy scope checkpoint changed candidate evidence')
        check(run(binary,'report','--session',legacy_path,'--json')['session']['search']['carve_scope']=='all','legacy scope changed')
        recover(legacy_path,legacy_rows,png(),'legacy-resumed-out')
        cases.extend(['process_loss_resume_preserves_scope_and_bytes','edited_checkpoint_scope_rejected','legacy_v1_checkpoint_keeps_all_scope'])
    proof={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
           'binary_sha256':file_hash(binary),'checks':len(cases),'cases':cases,'files_byte_exact':exact,'source_unchanged':True,
           'scope':'Independent synthetic NTFS $Bitmap, FAT32 mirrored FAT, exFAT bitmap and MBR/GPT. Allocation evidence is current ownership, never proof of deletion or original bytes. Read buffers still span allocated regions.'}
    if output:Path(output).write_text(json.dumps(proof,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(proof),flush=True);return proof


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
