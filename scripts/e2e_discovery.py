"""Lost tables and boot copies, validated alternatives and read-only restoration.

Only newly created regular images are altered; no host device is opened for writing.
"""
import argparse, hashlib, json, os, platform, shutil, struct, subprocess, tempfile
from pathlib import Path
from e2e import Client, check, run
from make_fixture import build, MFT_LCN, SECTOR, RECORD
from make_fat_fixture import fat32, exfat, file_hash
from e2e_disks import disk_image


def ntfs(path):
    truth=build(path)
    return {key:bytes.fromhex(value['hex']) for key,value in truth.items()},[],{}


def write(path,offset,data):
    with path.open('r+b') as f:f.seek(offset);f.write(data)


def verify(binary,root,name,kind,make,broken=False,mcp=False):
    volume=root/(name+'-volume.raw');truth,_,_=make(volume);source=root/(name+'.raw')
    starts=disk_image(source,[volume],scheme='mbr');offset=starts[0][0]*512
    write(source,0,bytes(512))
    if broken:write(source,offset,bytes(512))
    before=file_hash(source)
    found=run(binary,'discover','--source',source,'--json')
    check(len(found['volumes'])==1 and not found['truncated'],'lost volume was not uniquely validated: '+str(found))
    choice=found['volumes'][0];region=choice['region']
    check(region['offset']==offset and region['filesystem']==kind and region['length']==volume.stat().st_size,'incorrect lost bounds')
    check(not choice['matches_current_table'] and len(choice['evidence'])>=2 and not choice['overlaps'],'lost volume provenance')
    check(region['recovery_boot']['role']==('backup' if broken else 'primary'),'boot copy role')
    scanned=run(binary,'scan','--source',source,'--volume-id',choice['id'],'--work-dir',root/(name+'-work'),'--json')
    recovered=run(binary,'recover','--session',scanned['session'],'--all','--destination',root/(name+'-out'),'--json')['results']
    check({r['candidate_id'] for r in recovered}==set(truth),'missing recoverable file')
    for r in recovered:check(r['status']=='completed' and Path(r['path']).read_bytes()==truth[r['candidate_id']],'lost file content')
    if mcp:
        client=Client(binary,source,root/(name+'-mcp-work'),root/(name+'-mcp-out'))
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            args={'source':str(source),'request_id':'discover'};job=client.tool('discover_volumes',args)
            check(client.tool('discover_volumes',args)==job,'discovery idempotency')
            result=client.wait(job['job_id']);check(result['state']=='completed' and result['result']['volumes']==found['volumes'],'MCP discovery differs')
            job=client.wait(client.tool('start_scan',{'source':str(source),'recovered_volume':region,'request_id':'scan'})['job_id'])
            check(job['state']=='completed','MCP recovered volume scan');sid=job['result']['session_id']
            job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(root/(name+'-mcp-out')),'request_id':'recover'})['job_id'])
            check(job['state']=='completed' and len(job['result']['results'])==len(truth),'MCP recovered files missing')
            for r in job['result']['results']:check(r['status']=='completed' and Path(r['path']).read_bytes()==truth[r['candidate_id']],'MCP lost bytes')
            client.tool('discover_volumes',{'source':str(volume),'request_id':'outside'},error=True)
        finally:client.close()
    check(file_hash(source)==before,'source modified by discovery/restoration')
    return source,found,scanned,len(recovered)


def main(binary,output=None):
    checks=0;files=0
    with tempfile.TemporaryDirectory(prefix='rescue-discovery-') as tmp:
        root=Path(tmp)
        for kind,make in [('ntfs',ntfs),('fat32',fat32),('exfat',exfat)]:
            for broken in [False,True]:
                source,found,scanned,count=verify(binary,root,kind+'-'+str(broken),kind,make,broken,mcp=broken)
                checks+=1;files+=count*(2 if broken else 1)
        source,found,scanned,_=verify(binary,root,'tamper','ntfs',ntfs,True)
        session=json.loads(Path(scanned['session']).read_text(encoding="utf-8"));id=session['candidates'][0]['id']
        original=next(c for c in session['candidates'] if c['path_verified'] and c['size'])
        restored=run(binary,'recover','--session',scanned['session'],'--id',original['id'],'--original','--volume-root',root/'explicit-original','--json')['results'][0]
        expected=(root/'tamper-out'/original['original_path']).read_bytes()
        check(restored['status']=='completed' and Path(restored['path']).read_bytes()==expected,'single recovered region did not accept explicit unnumbered root');checks+=1;files+=1
        for key,value in [('offset',2**64-1),('length',2**64-1),('recovery_boot',{'offset':0,'bytes':512,'role':'primary','sha256':'0'*64})]:
            changed=json.loads(json.dumps(session));changed['region'][key]=value;path=root/('tampered-'+key+'.json');path.write_text(json.dumps(changed))
            run(binary,'recover','--session',path,'--id',id,'--destination',root/('tampered-out-'+key),'--json',expected=4);checks+=1
        at=found['volumes'][0]['region']['recovery_boot']['offset'];write(source,at+72,b'changed!')
        run(binary,'recover','--session',scanned['session'],'--id',id,'--destination',root/'changed-out','--json',expected=4);checks+=1
        # One correct boot with no MFT/root evidence is not accepted as a volume.
        fake=root/'fake.raw';build(fake);write(fake,MFT_LCN*SECTOR,bytes(64*RECORD))
        check(not run(binary,'discover','--source',fake,'--json')['volumes'],'boot signature alone accepted');checks+=1
        bad=root/'bounds.raw';build(bad)
        write(bad,40,struct.pack('<Q',2**64-1));write(bad,bad.stat().st_size-512+40,struct.pack('<Q',2**64-1))
        check(not run(binary,'discover','--source',bad,'--json')['volumes'],'overflowing volume accepted');checks+=1
        # Two valid, overlapping geometries remain explicit alternatives.
        outer=root/'overlap.raw';inner=root/'inner.raw';build(outer);build(inner);boot=bytearray(outer.read_bytes()[:512]);struct.pack_into('<Q',boot,40,4*1024*1024//512-1)
        write(outer,outer.stat().st_size-512,bytes(512))
        with outer.open('r+b') as f:f.truncate(4*1024*1024)
        write(outer,0,boot);write(outer,outer.stat().st_size-512,boot);write(outer,512*1024,inner.read_bytes())
        overlap=run(binary,'discover','--source',outer,'--json')
        check(len(overlap['volumes'])==2 and all(len(v['overlaps'])==1 for v in overlap['volumes']),'overlapping candidates silently merged')
        check(all(v['overlaps'][0] in {x['id'] for x in overlap['volumes']} for v in overlap['volumes']),'overlap references');checks+=1
        current=run(binary,'discover','--source',inner,'--json');check(len(current['volumes'])==1 and current['volumes'][0]['matches_current_table'],'current volume flag');checks+=1
        run(binary,'discover','--source',inner,'--range-start',1000,'--range-end',999,'--json',expected=2);checks+=1
        # The mirror supplies only record 0's runlist. It cannot replace other records.
        mirror=root/'mirror.raw';truth=build(mirror);raw=bytearray(mirror.read_bytes());mirror_at=800*SECTOR
        raw[mirror_at:mirror_at+RECORD]=raw[MFT_LCN*SECTOR:MFT_LCN*SECTOR+RECORD]
        struct.pack_into('<Q',raw,56,800);raw[-SECTOR:]=raw[:SECTOR];raw[MFT_LCN*SECTOR:MFT_LCN*SECTOR+RECORD]=bytes(RECORD);mirror.write_bytes(raw)
        before=file_hash(mirror);m=run(binary,'scan','--source',mirror,'--work-dir',root/'mirror-work','--json')
        check(any('MFT mirror' in w for w in m['warnings']),'MFT mirror provenance missing')
        rows=run(binary,'recover','--session',m['session'],'--all','--destination',root/'mirror-out','--json')['results']
        check({r['candidate_id'] for r in rows}==set(truth),'MFT mirror recovery missing files')
        for r in rows:check(r['status']=='completed' and file_hash(Path(r['path']))==truth[r['candidate_id']]['sha256'],'MFT mirror bytes')
        check(file_hash(mirror)==before,'MFT mirror repair wrote to source');checks+=1;files+=len(rows)
        write(mirror,MFT_LCN*SECTOR+20*RECORD,bytes(RECORD))
        partial=run(binary,'scan','--source',mirror,'--work-dir',root/'mirror-partial-work','--json')
        check(partial['candidates']==len(truth)-1,'mirror invented a missing non-mirrored MFT record');checks+=1
        write(mirror,mirror_at,bytes(RECORD))
        run(binary,'scan','--source',mirror,'--work-dir',root/'mirror-both-bad','--json',expected=4);checks+=1
        # Source allocation is sparse; cancellation must return partial coverage.
        large=root/'large.raw'
        with large.open('xb') as f:f.truncate(2*1024*1024*1024)
        client=Client(binary,large,root/'cancel-work',root/'cancel-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'});j=client.tool('discover_volumes',{'source':str(large),'request_id':'cancel'})
            client.tool('cancel_job',{'job_id':j['job_id']});result=client.wait(j['job_id']);check(result['state']=='cancelled' and result['result']['cancelled'],'discovery cancellation')
        finally:client.close()
        checks+=1
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'os':platform.system(),'checks':checks,'files_byte_exact':files,'source_unchanged':True,'scope':'Synthetic lost MBR, primary/backup boots for NTFS/FAT32/exFAT, false signatures, overflow, overlap alternatives, tampered session, changed source, CLI/MCP parity, bounded MFT mirror bootstrap and cancellation. No host device modified.'}
    if output:output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
