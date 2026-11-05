"""Windows NTFS creates/deletes 20 ADS on a newly owned VHD, then exact CLI/MCP recovery.

No host disk is formatted. OS-created native metadata is never edited by this test.
"""
import argparse, hashlib, json, os, platform, struct, subprocess, tempfile
from pathlib import Path
from e2e import Client, run, check
from e2e_restore_native import mounted_fixture, sync_volume


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
            kind,length=struct.unpack_from('<II',record,at)
            if kind==0xffffffff or length<24:break
            if kind==0x20:
                attr=record[at:at+length]
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
            at+=length
    return result

def main(binary,output):
    check(platform.system()=='Windows','native NTFS scenario requires Windows')
    output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='rescue-owned-ntfs-',dir=output.parent) as tmp:
        root=Path(tmp).resolve()
        with mounted_fixture(root) as (source,mount,filesystem):
            check(filesystem=='ntfs','NTFS fixture required')
            path=mount/'native-streams-日本語.pdf';truth={None:b'%PDF-1.4\n'+b'Native NTFS bytes\n'*512}
            for n in range(20):truth[f'stream-{n:02d}-Windows-created-extra-attribute']=bytes([n])*8192+f'ADS {n}'.encode()
            for name,payload in truth.items():
                target=path if name is None else Path(str(path)+':'+name)
                with target.open('xb') as f:f.write(payload);f.flush();os.fsync(f.fileno())
            # The last name deletion releases the file and its attribute extents.
            alias=mount/'native-alias.pdf';os.link(path,alias);path.unlink();alias.unlink();sync_volume(mount)
            # Save session and output on the runner volume, never onto the source fixture.
            scanned=run(binary,'scan','--source',source,'--work-dir',root/'work','--json')
            candidates=run(binary,'results','--session',scanned['session'],'--limit',1000,'--json')['candidates']
            found=[c for c in candidates if c['name'].split(':')[0] in [path.name,alias.name]]
            valid=len(found)==21 and {c['stream_name'] for c in found}==set(truth) and all(c['supported'] for c in found)
            diagnosis=None if valid else attribute_diagnostics(binary,source,root,found)
            check(valid,'native deleted attributes missing or unsupported: '+str(found)+'; diagnostic: '+str(diagnosis))
            result=run(binary,'recover','--session',scanned['session'],*[v for c in found for v in ['--id',c['id']]],'--destination',root/'out','--json')['results']
            names={c['id']:c['stream_name'] for c in found}
            check(len(result)==21 and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in result),'native ADS content mismatch')
            client=Client(binary,source,root/'mcp-work',root/'mcp-out')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                job=client.wait(client.tool('start_scan',{'source':str(source),'request_id':'scan'})['job_id']);check(job['state']=='completed',str(job))
                sid=job['result']['session_id']
                job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(names),'destination':str(root/'mcp-out'),'request_id':'recover'})['job_id'])
                check(job['state']=='completed' and len(job['result']['results'])==21 and {r['candidate_id'] for r in job['result']['results']}==set(names) and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in job['result']['results']),'MCP native ADS content mismatch')
            finally:client.close()
            result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'os':platform.system(),'actual_deletion':True,'native_ntfs':True,'cli_streams_byte_exact':21,'mcp_streams_byte_exact':21,'names_deduplicated':True,'scope':'20 OS-created ADS and the unnamed stream, deletion of both hard-link names, on a new owned VHD only; no metadata edited'}
        output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();main(a.binary.resolve(),a.output.resolve())
