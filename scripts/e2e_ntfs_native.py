"""Windows NTFS creates/deletes 20 ADS on a newly owned VHD, then exact CLI/MCP recovery.

No host disk is formatted. OS-created native metadata is never edited by this test.
"""
import argparse, hashlib, json, os, platform, re, struct, subprocess, tempfile
from contextlib import contextmanager
from pathlib import Path
from e2e import Client, run, check
from e2e_restore_native import mounted_fixture, sync_volume, call


@contextmanager
def ntfs_trim_policy(value):
    # Only a disposable GitHub-hosted VM may change this machine-wide NTFS
    # setting. The original value is restored even when a test fails.
    check(os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('RUNNER_ENVIRONMENT')=='github-hosted','TRIM profile requires a disposable GitHub-hosted test VM')
    def current():
        text=call('fsutil.exe','behavior','query','DisableDeleteNotify')
        match=re.search(r'^NTFS DisableDeleteNotify\s*=\s*([01])',text,re.M)
        check(match is not None,'NTFS TRIM setting unavailable: '+text)
        return int(match.group(1))
    original=current();proof={'original_disable_delete_notify':original,'test_disable_delete_notify':value,'restored':False}
    try:
        call('fsutil.exe','behavior','set','DisableDeleteNotify','NTFS',str(value))
        check(current()==value,'NTFS TRIM profile was not applied')
        yield proof
    finally:
        call('fsutil.exe','behavior','set','DisableDeleteNotify','NTFS',str(original))
        proof['restored']=current()==original
        check(proof['restored'],'NTFS TRIM profile was not restored')

def populate(mount,before_delete=None):
    path=mount/'native-streams-日本語.pdf';truth={None:b'%PDF-1.4\n'+b'Native NTFS bytes\n'*512}
    for n in range(20):truth[f'stream-{n:02d}-Windows-created-extra-attribute']=bytes([n])*8192+f'ADS {n}'.encode()
    for name,payload in truth.items():
        target=path if name is None else Path(str(path)+':'+name)
        with target.open('xb') as f:f.write(payload);f.flush();os.fsync(f.fileno())
    alias=mount/'native-alias.pdf';os.link(path,alias);path.unlink();sync_volume(mount)
    if before_delete:before_delete(alias)
    alias.unlink();sync_volume(mount)
    return path,alias,truth

def attribute_diagnostics(binary,source,root,candidates):
    """Read only the owned fixture through Rescue; bound failure diagnostics."""
    acquired=run(binary,'image','--source',source,'--destination',root/'diagnostic-image','--json')
    raw=(root/'diagnostic-image/image.rescue.raw').read_bytes()
    bps=struct.unpack_from('<H',raw,11)[0];cluster=bps*raw[13];mft=struct.unpack_from('<Q',raw,48)[0]*cluster;code=struct.unpack_from('<b',raw,64)[0];size=(1<<-code) if code<0 else code*cluster
    result=[]
    for candidate in candidates[:2]:
        record=bytearray(raw[mft+candidate['record']*size:mft+(candidate['record']+1)*size]);usa,count=struct.unpack_from('<HH',record,4)
        for n in range(1,count):record[n*bps-2:n*bps]=record[usa+2*n:usa+2*n+2]
        at=struct.unpack_from('<H',record,20)[0]
        while at+24<=len(record):
            kind,attr_length=struct.unpack_from('<II',record,at)
            if kind==0xffffffff or attr_length<24 or at+attr_length>len(record):break
            if kind==0x20:
                attr=record[at:at+attr_length]
                if attr[8]:
                    data_size=struct.unpack_from('<Q',attr,48)[0];pos=struct.unpack_from('<H',attr,32)[0];lcn=0;data=bytearray();runs=[]
                    while pos<len(attr) and attr[pos]:
                        header=attr[pos];pos+=1;a=header&15;b=header>>4
                        if not a or a>8 or b>8:break
                        count=int.from_bytes(attr[pos:pos+a],'little');pos+=a;lcn+=int.from_bytes(attr[pos:pos+b],'little',signed=True);pos+=b;runs.append([lcn,count]);data+=raw[lcn*cluster:(lcn+count)*cluster]
                    data=data[:data_size]
                else:
                    data_size,offset=struct.unpack_from('<IH',attr,16);data=attr[offset:offset+data_size];runs=[]
                rows=[];pos=0
                while pos+8<=len(data) and len(rows)<30:
                    kind,length=struct.unpack_from('<IH',data,pos);rows.append([pos,kind,length,bytes(data[pos:pos+32]).hex()])
                    if length<26 or pos+length>len(data):break
                    pos+=length
                result.append({'record':candidate['record'],'list_bytes':data_size,'runs':runs,'entries':rows,'nonzero_bytes':sum(x!=0 for x in data)})
            at+=attr_length
    return result

def recovery_diagnostics(results,names,truth):
    rows=[]
    for item in results:
        expected=truth[names[item['candidate_id']]]
        actual=Path(item['path']).read_bytes() if item.get('path') else None
        if item['status']!='completed' or actual!=expected:
            rows.append({'id':item['candidate_id'],'status':item['status'],'error':item.get('error'),'warnings':item.get('warnings'),
                'expected_bytes':len(expected),'actual_bytes':None if actual is None else len(actual),
                'expected_sha256':hashlib.sha256(expected).hexdigest(),'actual_sha256':None if actual is None else hashlib.sha256(actual).hexdigest(),
                'actual_prefix':None if actual is None else actual[:24].hex()})
    return rows[:4]

def main(binary,output):
    check(platform.system()=='Windows','native NTFS scenario requires Windows')
    output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='rescue-owned-ntfs-',dir=output.parent) as tmp:
        root=Path(tmp).resolve()
        with ntfs_trim_policy(1) as trim_policy, mounted_fixture(root) as (source,mount,filesystem):
            check(filesystem=='ntfs','NTFS fixture required')
            before=[]
            def inspect_before(path):
                snapshot=root/'before-delete';snapshot.mkdir()
                before.extend(attribute_diagnostics(binary,source,snapshot,[{'record':path.stat().st_ino & ((1<<48)-1)}]))
                check(before and all(d['nonzero_bytes']>0 for d in before),'OS-created attribute list was never durable: '+str(before))
            path,alias,truth=populate(mount,inspect_before)
            # Save session and output on the runner volume, never onto the source fixture.
            scanned=run(binary,'scan','--source',source,'--work-dir',root/'work','--json')
            candidates=run(binary,'results','--session',scanned['session'],'--limit',1000,'--json')['candidates']
            found=[c for c in candidates if c['name'].split(':')[0] in [path.name,alias.name]]
            valid=len(found)==21 and {c['stream_name'] for c in found}==set(truth) and all(c['supported'] for c in found)
            diagnosis=None if valid else attribute_diagnostics(binary,source,root,found)
            check(valid,'native deleted attributes missing or unsupported: '+str(found)+'; before-list-bytes: '+str([d['list_bytes'] for d in before])+'; diagnostic: '+str(diagnosis))
            result=run(binary,'recover','--session',scanned['session'],*[v for c in found for v in ['--id',c['id']]],'--destination',root/'out','--json')['results']
            names={c['id']:c['stream_name'] for c in found}
            check(len(result)==21 and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in result),'native ADS content mismatch: '+str(recovery_diagnostics(result,names,truth)))
            client=Client(binary,source,root/'mcp-work',root/'mcp-out')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                job=client.wait(client.tool('start_scan',{'source':str(source),'request_id':'scan'})['job_id']);check(job['state']=='completed',str(job))
                sid=job['result']['session_id']
                job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(names),'destination':str(root/'mcp-out'),'request_id':'recover'})['job_id'])
                check(job['state']=='completed' and len(job['result']['results'])==21 and {r['candidate_id'] for r in job['result']['results']}==set(names) and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in job['result']['results']),'MCP native ADS content mismatch')
            finally:client.close()
            result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'os':platform.system(),'actual_deletion':True,'native_ntfs':True,'cli_streams_byte_exact':21,'mcp_streams_byte_exact':21,'names_deduplicated':True,'trim_disabled_profile':trim_policy,'attribute_list_bytes_before_deletion':before[0]['list_bytes'],'attribute_list_nonzero_before_deletion':True,'scope':'20 OS-created ADS and the unnamed stream, deletion of both hard-link names, with NTFS TRIM notifications temporarily disabled on a disposable hosted VM; original policy restored; new owned VHD only, no metadata edited'}
        # A second actual deletion with notifications enabled records whether
        # this virtual backend erases the external attribute list. Never invent it.
        negative=root/'trim-enabled';negative.mkdir()
        with ntfs_trim_policy(0) as enabled_policy, mounted_fixture(negative) as (source,mount,filesystem):
            path,alias,truth=populate(mount)
            scanned=run(binary,'scan','--source',source,'--work-dir',negative/'work','--json')
            rows=run(binary,'results','--session',scanned['session'],'--limit',1000,'--json')['candidates']
            found=[c for c in rows if c['name'].split(':')[0] in [path.name,alias.name]]
            check(found,'deleted NTFS metadata unexpectedly absent')
            diagnosis=attribute_diagnostics(binary,source,negative,found)
            zeroed=bool(diagnosis) and all(d['list_bytes']>0 and d['nonzero_bytes']==0 for d in diagnosis)
            if zeroed:
                check(all(not c['supported'] for c in found),'erased list incorrectly marked recoverable')
            else:
                check(len(found)==21 and all(c['supported'] for c in found),'retained metadata is unsupported: '+str(found)+'; '+str(diagnosis))
                restored=run(binary,'recover','--session',scanned['session'],'--all','--destination',negative/'out','--json')['results'];names={c['id']:c['stream_name'] for c in found}
                check(len(restored)==21 and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in restored),'enabled-TRIM retained bytes mismatch')
            result['trim_enabled_profile']={'policy':enabled_policy,'attribute_list_zeroed':zeroed,'erased_metadata_not_fabricated':zeroed,'retained_streams_byte_exact':0 if zeroed else 21}
        output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();main(a.binary.resolve(),a.output.resolve())
