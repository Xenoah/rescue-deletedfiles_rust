"""Byte-exact PNG/JPEG carving and malformed entropy tests, using only stdlib."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import struct
import subprocess
import tempfile
import zipfile
from e2e import Client, check, run
from make_carve_fixture import png

CORPUS=Path(__file__).resolve().parent.parent/'tests/jpeg'


def corpus():
    entries=json.loads((CORPUS/'manifest.json').read_text(encoding='utf-8'))['files']
    files={e['file']:(CORPUS/e['file']).read_bytes() for e in entries}
    for e in entries:
        check(len(files[e['file']])==e['bytes'] and hashlib.sha256(files[e['file']]).hexdigest()==e['sha256'],'JPEG corpus changed')
    return files


def build(path):
    files=corpus()
    # Both PNG's eight-byte and JPEG's three-byte signatures cross a 1 MiB window.
    image=bytearray(1024*1024-2)
    truth={}
    for name in ['comment-fill.jpg',*[n for n in files if n!='comment-fill.jpg']]:
        at=len(image);data=files[name];image.extend(data);image.extend(bytes(37))
        truth[f'jpeg-{at:016x}']=data
        if name=='embedded-thumbnail.jpg':
            at+=data.index(files['edge-1x1-0.jpg'])
            truth[f'jpeg-{at:016x}']=files['edge-1x1-0.jpg']
    image.extend(bytes(2*1024*1024-3-len(image)))
    truth[f'png-{len(image):016x}']=png()
    image.extend(png());image.extend(bytes(5*1024*1024-len(image)))
    path.write_bytes(image)
    return truth


def main(binary, output=None):
    cases=0;files=corpus()
    with tempfile.TemporaryDirectory(prefix='rescue-jpeg-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth=build(source)
        initial=hashlib.sha256(source.read_bytes()).hexdigest()

        def scan(image, label, *extra, expected=0):
            return run(binary,'scan','--mode','carve','--source',image,'--work-dir',root/(label+'-work'),*extra,'--json',expected=expected)

        summary=scan(source,'mixed')
        session=json.loads(Path(summary['session']).read_text(encoding='utf-8'))
        check(session['schema_version']==6,'JPEG sessions need schema 6')
        check(summary['candidates']==len(truth) and summary['invalid_records']==0,summary)
        check(summary['scanned_records']==source.stat().st_size and summary['progress_unit']=='bytes',summary)
        check({c['id'] for c in session['candidates']}==set(truth),session)
        for c in session['candidates']:
            check(c['supported'] and not c['path_verified'] and c['allocation_status']=='unknown' and c.get('created') is None and c.get('modified') is None,c)
            check(c['extent_evidence']==('jpeg_validated' if c['id'].startswith('jpeg') else 'png_validated'),c)
        restored=run(binary,'recover','--session',summary['session'],'--all','--destination',root/'out','--json')['results']
        for item in restored:
            data=truth[item['candidate_id']]
            check(item['status']=='completed' and Path(item['path']).read_bytes()==data and item['sha256']==hashlib.sha256(data).hexdigest(),item)
        cases+=1
        # Aliases act on format detection before validation, so unwanted invalid
        # signatures do not spend the selected format's scan budget.
        for extension in ['jpg','jpeg','jpe','*.JPEG','.jpg','png','pdf','*','jpg,png']:
            selected=scan(source,'ext-'+str(cases),'--extension',extension)
            expected=1 if extension=='png' else 0 if extension=='pdf' else len(truth) if extension in ['*','jpg,png'] else len(truth)-1
            check(selected['candidates']==expected,(extension,selected));cases+=1
        first=session['candidates'][0];at=first['record'];end=at+len(truth[first['id']])
        for label,start,stop,count,invalid in [('exact',at,end,1,0),('cut',at,end-1,1,1),('skip',at+1,end,0,0),('empty',at,at,0,0)]:
            result=scan(source,'range-'+label,'--range-start',start,'--range-end',stop,expected=5 if invalid else 0)
            check(result['candidates']==count and result['invalid_records']==invalid,result);cases+=1
        # Raw carving cannot invent an original path, even when explicitly given a root.
        original=run(binary,'recover','--session',summary['session'],'--id',first['id'],'--original','--volume-root',root,'--json',expected=5)
        check(original['results'][0]['path'] is None,original);cases+=1
        kept=run(binary,'recover','--session',summary['session'],'--id',first['id'],'--destination',root/'out','--json')['results'][0]
        check(Path(kept['path']).read_bytes()==truth[first['id']] and Path(kept['path']).name!=first['name'],kept);cases+=1
        skipped=run(binary,'recover','--session',summary['session'],'--id',first['id'],'--destination',root/'out','--collision','skip','--json')['results'][0]
        check(skipped['status']=='skipped' and skipped['bytes_written']==0,skipped);cases+=1
        archive=root/'photos.zip'
        result=run(binary,'recover','--session',summary['session'],'--all','--archive',archive,'--json')
        with zipfile.ZipFile(archive) as z:
            check(z.testzip() is None,'ZIP CRC')
            report=json.loads(z.read('rescue-report.json'))
            check(len(report['results'])==len(truth),report)
            for item in report['results']:check(z.read(item['archive_entry'])==truth[item['candidate_id']],item)
        cases+=1

        # Parse precise segment positions from valid encoded fixtures. Entropy
        # may contain stuffed bytes and RST markers, neither ends a scan.
        def segments(data):
            values=[];i=2
            while i<len(data):
                if data[i]!=255: i+=1;continue
                start=i;i+=1
                while i<len(data) and data[i]==255:i+=1
                if i==len(data):break
                code=data[i];i+=1
                if code==0 or 0xd0<=code<=0xd7:continue
                if code==0xd9:values.append((start,code,i));break
                length=int.from_bytes(data[i:i+2],'big');i+=length
                values.append((start,code,i))
            return values

        base=files['separate-1.jpg'];sof=base.index(b'\xff\xc0');sos=base.index(b'\xff\xda');entropy=sos+2+int.from_bytes(base[sos+2:sos+4],'big')
        prog=files['rgb-1-2.jpg'];ps=segments(prog);first_scan=next((start,end) for start,code,end in ps if code==0xda);next_marker=next(start for start,code,end in ps if start>first_scan[0])
        broken={
            'no-frame':b'\xff\xd8\xff\xd9',
            'header-only':base[:entropy],
            'missing-eoi':base[:-2],
            'entropy-missing':base[:entropy]+b'\xff\xd9',
            'entropy-extra':base[:-2]+b'\x00'+base[-2:],
            'padding-zero':base[:entropy]+b'\x0e'+base[entropy+1:],
            'stuffing-missing':base[:entropy]+b'\xff\x01'+base[entropy+1:],
            'stuffing-excess':base[:entropy]+b'\xff\x00'+base[entropy+1:],
            'early-eoi-large':files['edge-127x65-0.jpg'][:files['edge-127x65-0.jpg'].index(b'\xff\xda')+14]+b'\xff\xd9',
            'progressive-only-first-scan':prog[:next_marker]+b'\xff\xd9',
            'progressive-cut-entropy':prog[:first_scan[1]+1]+b'\xff\xd9',
            'restart-without-dri':base[:entropy]+b'\xff\xd0'+base[entropy:],
            'zero-length':b'\xff\xd8\xff\xfe\x00\x00',
            'past-boundary':b'\xff\xd8\xff\xfe\xff\xff'+bytes(17),
            'extra-header-bytes':base[:sof]+b'junk'+base[sof:],
            'duplicate-frame':base[:sof]+base[sof:sof+13]+base[sof:],
            'lossless':base[:sof+1]+b'\xc3'+base[sof+2:],
            'arithmetic':base[:sof+1]+b'\xc9'+base[sof+2:],
            'unknown-marker':base[:2]+b'\xff\xf7\x00\x02'+base[2:],
        }
        def altered(data,offset,value):
            b=bytearray(data);b[offset:offset+len(value)]=value;return bytes(b)
        broken.update({
            'precision12':altered(base,sof+4,b'\x0c'),
            'zero-height':altered(base,sof+5,b'\x00\x00'),
            'huge-pixels':altered(base,sof+5,b'\xff\xff\xff\xff'),
            'zero-sampling':altered(base,sof+11,b'\x00'),
            'missing-quant':altered(base,sof+12,b'\x03'),
            'zero-quant':altered(base,7,b'\x00'),
            'bad-scan-table':altered(base,sos+6,b'\x33'),
            'unknown-component':altered(base,sos+5,b'\x03'),
            'sequential-band':altered(base,sos+7,b'\x01'),
        })
        dht=base.index(b'\xff\xc4');broken['huffman-all-ones']=altered(base,dht+5,b'\x02')
        restart=files['restart-3-0.jpg'];rst=restart.index(b'\xff\xd0');dri=restart.index(b'\xff\xdd')
        broken['restart-order']=altered(restart,rst+1,b'\xd2')
        broken['restart-interval']=altered(restart,dri+4,b'\x00\x01')
        broken['restart-missing']=restart[:rst]+restart[rst+2:]
        broken['missing-component']=files['separate-3.jpg'][:next(end for start,code,end in segments(files['separate-3.jpg']) if code==0xda)+1]+b'\xff\xd9'
        for label,data in broken.items():
            image=root/(label+'.raw');image.write_bytes(data)
            bad=scan(image,'bad-'+label,expected=5)
            candidates=json.loads(Path(bad['session']).read_text(encoding='utf-8'))['candidates']
            candidate=next(c for c in candidates if c['record']==0)
            check(not candidate['supported'],(label,candidate))
            recovery=run(binary,'recover','--session',bad['session'],'--id',candidate['id'],'--destination',root/(label+'-out'),'--json',expected=5)['results'][0]
            check(recovery['path'] is None and recovery['sha256'] is None,(label,recovery));cases+=1

        # Serialized name and length are never authority for raw data or output.
        for key,value in [('record',2**64-1),('record_sha256','0'*64),('size',2**64-1),('name','../../outside.jpg'),('extent_evidence','png_validated')]:
            forged=json.loads(json.dumps(session));forged['candidates'][0][key]=value
            path=root/(key+'.json');path.write_text(json.dumps(forged),encoding='utf-8')
            reject=key in ['record','record_sha256']
            result=run(binary,'recover','--session',path,'--id',first['id'],'--destination',root/(key+'-out'),'--json',expected=5 if reject else 0)['results'][0]
            check(result['path'] is None if reject else Path(result['path']).read_bytes()==truth[first['id']] and result['extent_evidence']=='jpeg_validated',result);cases+=1
        # Modify only an APP/COM byte outside sampled fingerprints, retaining mtime.
        saved=source.read_bytes();stat=source.stat();changed=bytearray(saved)
        changed[at+truth[first['id']].index(b'comment')]=ord('C')
        source.write_bytes(changed);os.utime(source,ns=(stat.st_atime_ns,stat.st_mtime_ns))
        result=run(binary,'recover','--session',summary['session'],'--id',first['id'],'--destination',root/'changed-out','--json',expected=5)['results'][0]
        check(result['path'] is None and 'SOURCE_CHANGED' in result['error'],result)
        source.write_bytes(saved);os.utime(source,ns=(stat.st_atime_ns,stat.st_mtime_ns));cases+=1

        client=Client(binary,source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            job=client.wait(client.tool('start_scan',{'source':str(source),'mode':'carve','extensions':['jpeg'],'request_id':'jpeg'})['job_id'])
            check(job['state']=='completed' and job['progress']['unit']=='bytes',job)
            sid=job['result']['session_id'];listed=client.tool('list_candidates',{'session_id':sid})['candidates']
            jpeg_ids=[key for key in truth if key.startswith('jpeg')]
            check({c['id'] for c in listed}==set(jpeg_ids),listed)
            job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':jpeg_ids,'destination':str(root/'mcp-out'),'request_id':'restore'})['job_id'])
            check(job['state']=='completed',job)
            for item in job['result']['results']:check(Path(item['path']).read_bytes()==truth[item['candidate_id']],item)
            cases+=1
        finally:client.close()
        check(hashlib.sha256(source.read_bytes()).hexdigest()==initial,'source modified')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
            'platform':platform.platform(),'checks':cases,'corpus_jpegs':len(files),'jpeg_candidates_exact':len(truth)-1,
            'mixed_png_exact':1,'malformed_cases':len(broken),'cli_mcp_byte_exact':True,'source_unchanged':True,
            'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
            'scope':'Synthetic contiguous images; independent libjpeg/Pillow corpus and minimal encoder, no physical deletion or general recovery-rate claim.'}
    if output:
        output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result),flush=True)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
