"""Independent mkntfs quick-format on a new regular image, with no metadata edits.

The unchanged geometry profile preserves 100 former resident/nonresident files.
This is ntfs-3g behavior on Linux, not Windows format.exe or an SSD TRIM claim.
"""
import argparse,hashlib,json,subprocess,tempfile
from pathlib import Path
from independent_ntfs import command
from e2e import Client,check,run

def main(binary,output=None):
    with tempfile.TemporaryDirectory(prefix='rescue-owned-quickformat-') as temporary:
        root=Path(temporary);image=root/'owned.raw';data=root/'input.bin'
        with image.open('xb') as f:f.truncate(64*1024*1024)
        command('mkntfs','-F','-Q','-s',512,'-c',4096,image)
        truth={}
        for n in range(100):
            name=f'old-document-{n:03d}.txt'
            value=(f'Residual document {n}\n'.encode()*(1 if n<50 else 1024))
            truth[name]=value;data.write_bytes(value);command('ntfscp',image,data,'/'+name)
        command('mkntfs','-F','-Q','-s',512,'-c',4096,image)
        before=hashlib.sha256(image.read_bytes()).hexdigest()
        scanned=run(binary,'scan','--source',image,'--work-dir',root/'work','--fragments','--json')
        rows=run(binary,'results','--session',scanned['session'],'--limit',1000,'--json')['candidates']
        check(len(rows)==100 and {r['name'] for r in rows}==set(truth),'independent formatted file enumeration')
        check(all(r['supported'] and not r['path_verified'] and r['extent_evidence']=='metadata_remnant' for r in rows),'formatted evidence mislabeled')
        names={r['id']:r['name'] for r in rows}
        results=run(binary,'recover','--session',scanned['session'],'--all','--destination',root/'out','--json')['results']
        check(len(results)==100 and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in results),'independent quick-format bytes differ')
        client=Client(binary,image,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            job=client.wait(client.tool('start_scan',{'source':str(image),'fragments':True,'request_id':'scan'})['job_id'])
            check(job['state']=='completed','MCP formatted scan')
            job=client.wait(client.tool('recover_files',{'session_id':job['result']['session_id'],'ids':list(names),'destination':str(root/'mcp-out'),'request_id':'recover'})['job_id'])
            check(job['state']=='completed' and len(job['result']['results'])==100,'MCP formatted result count')
            check(all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in job['result']['results']),'MCP formatted bytes differ')
        finally:client.close()
        check(hashlib.sha256(image.read_bytes()).hexdigest()==before,'quick-format recovery modified source')
    proof={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'actual_quickformat':True,'metadata_edited':False,'cli_files_byte_exact':100,'mcp_files_byte_exact':100,'resident_files':50,'nonresident_files':50,'source_unchanged':True,'scope':__doc__.strip()}
    if output:output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(proof,indent=2)+'\n')
    print(json.dumps(proof));return proof
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
