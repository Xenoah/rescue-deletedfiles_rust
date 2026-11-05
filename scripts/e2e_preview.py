"""Real isolated-worker preview, independent pixels/text, permissions and bounds."""
import argparse, hashlib, io, json, os, signal, struct, subprocess, tempfile, time, zlib
from pathlib import Path
from PIL import Image
from e2e import run, check, Client
from e2e_formats import corpus_load, ntfs_image, fat_image, file_hash


def entry(name,data):
    return {'path':Path(name),'file':name,'data':data,'bytes':len(data),
            'sha256':hashlib.sha256(data).hexdigest(),'format':Path(name).suffix[1:],'extension':Path(name).suffix[1:]}


def main(binary,output=None):
    cases=[];images=0;texts=0
    corpus=list(corpus_load(Path(__file__).resolve().parent.parent/'tests/corpus'))
    basic=[x for x in corpus if x['path'].name in ['sample.png','sample.jpg','sample.txt']]
    with tempfile.TemporaryDirectory(prefix='rescue-preview-') as temporary:
        root=Path(temporary)
        canvas=Image.new('RGBA',(1024,256),(37,91,163,129));buf=io.BytesIO();canvas.save(buf,format='PNG')
        large=entry('large.png',buf.getvalue())
        utf16=entry('日本語.txt',b'\xff\xfe'+'削除した文書\n<unsafe> is literal'.encode('utf-16le'))
        long_text=entry('long.txt',b'x'*65535+'あ'.encode()+b' rest')
        for fs in ['ntfs','fat32','exfat']:
            source=root/(fs+'.raw');files=[*basic,large,utf16,long_text]
            truth,_=ntfs_image(source,files,'contiguous') if fs=='ntfs' else fat_image(source,files,fs,False)
            before=file_hash(source)
            scanned=run(binary,'scan','--indexed','--source',source,'--work-dir',root/(fs+'-work'),'--json')
            for candidate,item in truth.items():
                result=run(binary,'preview','--session',scanned['session'],'--id',candidate,
                           '--destination',root/(fs+'-previews'),'--json')
                data=Path(result['path']).read_bytes();info=result['preview']
                check(hashlib.sha256(data).hexdigest()==result['sha256'],'preview export checksum')
                check(info['source_bytes_read']<=64*1024*1024 and info['candidate_id']==candidate,info)
                if item['path'].suffix in ['.png','.jpg']:
                    with Image.open(io.BytesIO(data)) as actual,Image.open(io.BytesIO(item['data'])) as expected:
                        actual=actual.convert('RGBA');expected=expected.convert('RGBA')
                        if item['path'].name=='large.png':expected=expected.resize((512,128),Image.Resampling.NEAREST)
                        check(actual.size==expected.size and info['width']==actual.width,info)
                        # JPEG libraries may round individual samples differently.
                        diffs=[abs(a-b) for a,b in zip(actual.tobytes(),expected.tobytes())]
                        check(max(diffs,default=0)<=(3 if item['path'].suffix=='.jpg' else 0),'independent decoder pixels differ')
                    images+=1
                else:
                    expected=item['data'].decode('utf-16') if item['path'].name=='日本語.txt' else item['data'][:65536].decode('utf-8',errors='ignore')
                    check(data.decode('utf-8')==expected,'literal text changed')
                    check(info['truncated']==(item['path'].name=='long.txt'),'text truncation not reported');texts+=1
            cases.append(fs+'_preview_pixels_text_encoding_prefix_and_thumbnail')
            client=Client(binary,source,root/(fs+'-work'),root/(fs+'-mcp'))
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                sid=client.tool('open_session',{'path':scanned['session']})['session_id']
                selected=next(key for key,item in truth.items() if item['path'].name=='日本語.txt')
                args={'session_id':sid,'candidate_id':selected,'destination':str(root/(fs+'-mcp')),'request_id':'preview'}
                job=client.tool('preview_candidate',args)
                check(client.tool('preview_candidate',args)==job,'preview request duplicated')
                done=client.wait(job['job_id']);check(done['state']=='completed',done)
                check('rgba' not in done['result'] and 'text' not in done['result'],'MCP leaked content')
                check(Path(done['result']['path']).read_bytes().decode('utf-8')==utf16['data'].decode('utf-16'),'MCP text differs');texts+=1
                client.tool('preview_candidate',{**args,'request_id':'outside','destination':str(root/'outside')},error=True)
                client.tool('preview_candidate',{**args,'candidate_id':'unknown'},error=True)
                terminal=client.tool('pause_job',{'job_id':job['job_id']})
                check(terminal['state']=='completed' and not terminal['details']['pause_supported'],'terminal preview state changed on pause')
                check(not (root/'outside').exists(),'outside output created')
            finally:client.close()
            check(file_hash(source)==before,'preview modified source')
            cases.append(fs+'_mcp_dedup_allowlist_no_contents_and_nonresumable')

        # Metadata recovery can support a file whose preview is unsupported.
        png=bytearray(next(x['data'] for x in basic if x['path'].suffix=='.png'))
        struct.pack_into('>II',png,16,65535,65535)
        struct.pack_into('>I',png,29,zlib.crc32(png[12:29])&0xffffffff)
        malformed=[entry('bomb.png',bytes(png)),entry('corrupt.jpg',b'\xff\xd8bad'),
                   entry('binary.txt',b'\x00\x01bad'),entry('odd.txt',b'\xff\xfe\x00'),
                   entry('document.pdf',b'%PDF-1.7\npreview unsupported')]
        source=root/'bad.raw';truth,_=ntfs_image(source,malformed,'contiguous');before=file_hash(source)
        scan=run(binary,'scan','--source',source,'--work-dir',root/'bad-work','--json')
        for key in truth:
            result=run(binary,'preview','--session',scan['session'],'--id',key,'--destination',root/'bad-out','--json',expected=4)
            check(json.loads(result)['error']['code'] in ['RESOURCE_LIMIT','INVALID_IMAGE','UNSUPPORTED'],result)
        check(not (root/'bad-out').exists(),'failed preview left an output')
        cases.append('pixel_bomb_corrupt_image_binary_and_unsupported_type_rejected')
        # Forged saved names must not let arbitrary bytes masquerade as the original.
        raw=json.loads(Path(scan['session']).read_text());raw['candidates'][0]['record_sha256']='0'*64
        changed=root/'changed.json';changed.write_text(json.dumps(raw))
        error=run(binary,'preview','--session',changed,'--id',raw['candidates'][0]['id'],'--destination',root/'changed-out','--json',expected=4)
        check(json.loads(error)['error']['code']=='SOURCE_CHANGED',error)
        check(file_hash(source)==before,'malformed preview modified input')
        cases.append('forged_metadata_revalidated_before_decoding')

        # A blocking source open exercises the actual parent watchdog/cancellation
        # without a production test hook. The FIFO is private and disposable.
        if os.name!='nt':
            fifo=root/'blocked.raw';os.mkfifo(fifo)
            raw['source']=str(fifo);blocked=root/'blocked.json';blocked.write_text(json.dumps(raw))
            start=time.monotonic();error=run(binary,'preview','--session',blocked,'--id',raw['candidates'][0]['id'],
                '--destination',root/'blocked-out','--json',expected=4,timeout=12)
            elapsed=time.monotonic()-start
            check(json.loads(error)['error']['code']=='RESOURCE_LIMIT' and 4<=elapsed<10,(elapsed,error))
            process=subprocess.Popen([str(binary),'preview','--session',str(blocked),'--id',raw['candidates'][0]['id'],
                '--destination',str(root/'cancelled-out'),'--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            time.sleep(.3);start=time.monotonic();process.send_signal(signal.SIGINT);process.communicate(timeout=5)
            check(process.returncode==130 and time.monotonic()-start<2,'preview cancel did not stop worker')
            check(not (root/'blocked-out').exists() and not (root/'cancelled-out').exists(),'cancelled preview published output')
            cases.extend(['actual_blocking_open_wall_time_watchdog','actual_worker_cancellation'])

        # A carved PNG uses the same isolated process, with content revalidation.
        carved=root/'carved.raw';carved.write_bytes(b'\x00'*4096+large['data']+b'\x00'*4096)
        scan=run(binary,'scan','--indexed','--source',carved,'--mode','carve','--extension','png','--work-dir',root/'carved-work','--json')
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates'];check(len(rows)==1,rows)
        result=run(binary,'preview','--session',scan['session'],'--id',rows[0]['id'],'--destination',root/'carved-out','--json')
        with Image.open(result['path']) as actual:check(actual.size==(512,128) and actual.convert('RGBA').getpixel((0,0))==(37,91,163,129),'carved preview pixels')
        images+=1;cases.append('carved_source_revalidation_and_pixels')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
            'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':len(cases),'cases':cases,
            'image_previews_verified':images,'text_previews_verified':texts,'source_unchanged':True,
            'scope':'Synthetic NTFS/FAT32/exFAT and carving. PNG exact RGBA; JPEG independent decoder max channel difference 3. Actual child-process wall-time and cancellation tested with Unix FIFO only. Not a claim of a complete OS security sandbox.'}
    if output:Path(output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result),flush=True);return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
