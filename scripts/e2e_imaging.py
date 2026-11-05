"""Actual CLI/MCP acquisition, process-kill resume and strict read-map propagation.

All sources are disposable regular files. Synthetic map faults are reported as
such; native disk/device behavior is covered by the owned-volume CI suite.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import struct
import subprocess
import tempfile
import zlib
from e2e import Client, check, run
from make_fixture import build


def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def mark_gap(image, offset):
    """Independent writer of one Failed record, simulating a permanently bad sector."""
    path=Path(str(image)+'.rmap');data=bytearray(path.read_bytes())
    check(data[:8]==b'RSCMAP01','read-map signature')
    cursor=12+struct.unpack_from('<I',data,8)[0]+32
    for at in range(cursor,len(data),76):
        start,length=struct.unpack_from('<QQ',data,at)
        if start<=offset<start+length:
            check(data[at+16]==1,'expected copied block')
            data[at+16]=2;struct.pack_into('<i',data,at+20,-1);data[at+24:at+56]=bytes(32)
            struct.pack_into('<I',data,at+72,zlib.crc32(data[at:at+72]))
            path.write_bytes(data)
            return start,length
    raise AssertionError('gap outside acquisition')


def main(binary, output=None):
    cases=[]
    with tempfile.TemporaryDirectory(prefix='rescue-imaging-') as tmp:
        root=Path(tmp);source=root/'source.raw';truth=build(source);before=sha(source)
        acquired=run(binary,'image','--source',source,'--destination',root/'image','--block-size','4096','--json')
        image=Path(acquired['image']);check(acquired['state']=='completed' and sha(image)==before,'acquisition not byte-exact');cases.append('complete image hash')
        summary=run(binary,'read-map','--image',image,'--json');check(summary['copied_bytes']==source.stat().st_size and summary['failed_bytes']==summary['unattempted_bytes']==0,'coverage');cases.append('coverage accounting')
        page=run(binary,'read-map','--image',image,'--limit','2','--json');check(len(page['ranges'])==2 and all(r['read_unix_ms']>0 and r['attempts']==1 and len(r['sha256'])==64 for r in page['ranges']),'read history')
        next_page=run(binary,'read-map','--image',image,'--limit','2','--offset',page['next_offset'],'--map-digest',page['map_digest'],'--json');check(next_page['ranges'][0]['offset']>page['ranges'][-1]['offset'],'unstable map pages')
        run(binary,'read-map','--image',image,'--map-digest','wrong-digest','--json',expected=4);cases.append('bounded read history and stable pagination')
        scan=run(binary,'scan','--source',image,'--work-dir',root/'scan','--json')
        restored=run(binary,'recover','--session',scan['session'],'--all','--destination',root/'restored','--json')
        for item in restored['results']:
            check(item['status']=='completed' and sha(Path(item['path']))==truth[item['candidate_id']]['sha256'],'mapped payload mismatch')
        cases.append('metadata scan and byte-exact restoration from acquisition')
        # Reopening a complete image does not append records or change the data.
        journal=Path(acquired['read_map']);journal_hash=sha(journal)
        run(binary,'image','--source',source,'--destination',root/'image','--block-size','4096','--resume','--json')
        check(sha(journal)==journal_hash and sha(image)==before,'completed resume changed durable data');cases.append('completed resume is idempotent')
        # Hard-kill a real writer after a durable progress event, then resume in another process.
        large=root/'large.raw'
        with large.open('wb') as f:
            for n in range(64):f.write(bytes([n])*1024*1024)
        large_before=sha(large)
        command=[str(binary),'image','--source',str(large),'--destination',str(root/'interrupted'),'--block-size','262144','--progress','--json']
        child=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        try:
            progress=json.loads(child.stderr.readline());check(progress['map']['copied_bytes']>0,'no durable checkpoint')
            child.kill();child.communicate(timeout=15)
        finally:
            if child.poll() is None:child.kill();child.wait(timeout=15)
        partial=root/'interrupted/image.rescue.raw';partial_map=Path(str(partial)+'.rmap')
        summary=run(binary,'read-map','--image',partial,'--json');check(0<summary['copied_bytes']<summary['length'],'fixture completed before kill')
        with partial_map.open('ab') as f:f.write(b'incomplete record')
        check(run(binary,'read-map','--image',partial,'--json')['ignored_incomplete_tail'],'crash tail not recognized')
        result=run(binary,'image','--source',large,'--destination',root/'interrupted','--block-size','262144','--resume','--json')
        check(result['state']=='completed' and sha(partial)==large_before==sha(large),'process-kill resume mismatch');cases.append('hard process kill, torn tail and resume')
        # Source timestamp/identity changes are rejected before adding any map records.
        saved=sha(partial_map);previous_mtime=large.stat().st_mtime_ns;os.utime(large,ns=(large.stat().st_atime_ns,previous_mtime+2_000_000_000))
        result=subprocess.run(command+['--resume'],capture_output=True,text=True,encoding='utf-8',timeout=30)
        check(result.returncode!=0 and 'SOURCE_CHANGED' in result.stderr and sha(partial_map)==saved,'changed source accepted');cases.append('changed source rejected')
        os.utime(large,ns=(large.stat().st_atime_ns,previous_mtime))
        # Mark a nonresident payload block Failed while its real RAW bytes still exist.
        needle=bytes.fromhex(truth['mft-21-7']['hex']);at=source.read_bytes().find(needle[:512]);check(at>=0,'nonresident fixture')
        gap_start,gap_length=mark_gap(image,at)
        scan=run(binary,'scan','--source',image,'--work-dir',root/'gap-scan','--json',expected=5)
        result=run(binary,'recover','--session',scan['session'],'--id','mft-21-7','--destination',root/'gap-out','--json',expected=5)['results'][0]
        check(result['status']=='incomplete' and result['path'] is None and result['missing_ranges'],'gap invented data')
        check(all(g['source_offset']>=gap_start and g['source_offset']<gap_start+gap_length and g['logical_offset'] is not None for g in result['missing_ranges']),'gap coordinates')
        cases.append('failed block propagated to file logical ranges, no output')
        # Damaged data with a good map is detected; map deletion cannot make reserved RAW trusted.
        with partial.open('r+b') as f:f.seek(17);f.write(b'X')
        result=subprocess.run(command+['--resume'],capture_output=True,text=True,encoding='utf-8',timeout=30)
        check(result.returncode!=0 and 'checksum mismatch' in result.stderr,'corrupt resume accepted');cases.append('resume rejects corrupted acquired bytes')
        image_map=Path(str(image)+'.rmap');backup=image_map.read_bytes();image_map.unlink()
        run(binary,'scan','--source',image,'--work-dir',root/'no-map','--json',expected=4)
        image_map.write_bytes(backup);cases.append('required sidecar cannot be silently omitted')
        client=Client(binary,source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2024-11-05'})
            arguments={'source':str(source),'destination':str(root/'mcp-out/acquired'),'request_id':'image-1'}
            first=client.tool('start_image',arguments);duplicate=client.tool('start_image',arguments)
            check(first['job_id']==duplicate['job_id'],'duplicate image job')
            job=client.wait(first['job_id']);check(job['state']=='completed' and sha(Path(job['result']['image']))==before,'MCP acquisition')
            generated=job['result']['image'];check(client.tool('get_read_map',{'image':generated})['unattempted_bytes']==0,'MCP map')
            scan=client.wait(client.tool('start_scan',{'source':generated,'request_id':'scan-image'})['job_id']);check(scan['state']=='completed','generated image not scannable')
            cases.append('MCP acquire, idempotency, map and generated-image scan')
            forbidden=client.wait(client.tool('start_image',{'source':str(source),'destination':str(root/'outside'),'request_id':'outside'})['job_id'])
            check(forbidden['state']=='failed' and not (root/'outside').exists(),'MCP escaped output root');cases.append('MCP output confinement')
            client.tool('start_image',{'source':str(large),'destination':str(root/'mcp-out/no'),'request_id':'unapproved'},error=True);cases.append('MCP source allowlist')
        finally:client.close()
        client=Client(binary,large,root/'cancel-work',root/'cancel-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            started=client.tool('start_image',{'source':str(large),'destination':str(root/'cancel-out/acquired'),'block_size':4096,'request_id':'cancel-image'})
            client.tool('cancel_job',{'job_id':started['job_id']});job=client.wait(started['job_id'])
            check(job['state']=='cancelled' and job['result']['summary']['unattempted_bytes']>0,'acquisition cancellation not observed');cases.append('MCP cooperative cancellation preserves unread ranges')
        finally:client.close()
        check(sha(source)==before,'source changed');cases.append('original source unchanged')
    report={'status':'passed','schema_version':1,'platform':platform.platform(),'version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':sha(binary),'cases':cases,'checks':len(cases),'scope':'Actual CLI/MCP processes and regular-file fixtures; failed ranges simulated by an independent read-map record editor. No native hardware fault claim.'}
    if output:output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(report));return report


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
