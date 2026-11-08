"""Explicit CP932 FAT 8.3 names, raw provenance and durable settings.

Uses Python's independent CP932 codec and ordinary FAT12/16/32 fixtures. Deleted
leading bytes remain unknown; ambiguous DBCS boundaries are never picked silently.
"""
import argparse,contextlib,csv,hashlib,io,json,sqlite3,struct,subprocess,tempfile,zipfile
from pathlib import Path
from e2e import Client,check,run,candidate_evidence
from e2e_disks import disk_image
import jsonschema
from e2e_classic_fat import make as classic
from e2e_index_fat import pages,large
from e2e_reports import Document
from make_fat_fixture import fat32,sfn,lfn,file_hash


def make(path,bits):
    if bits == 32:
        _,_,meta=fat32(path);heap=meta['heap'];root=heap;root_size=512;folder=9
    else:
        _,_,meta=classic(path,bits);heap=meta['heap'];root=meta['root'];root_size=512*32;folder=7
    data=bytearray(path.read_bytes());sector=512;reserved=struct.unpack_from('<H',data,14)[0]
    fat_sectors=struct.unpack_from('<I' if bits==32 else '<H',data,36 if bits==32 else 22)[0]
    data[root:root+root_size]=bytes(root_size)
    if bits==32:data[heap+6*512:heap+7*512]=bytes(512)
    address=lambda c:heap+(c-2)*sector
    def end_chain(c):
        for copy in range(2):
            at=(reserved+copy*fat_sectors)*512
            if bits==12:
                at+=c*3//2;old=struct.unpack_from('<H',data,at)[0]
                struct.pack_into('<H',data,at,(old&15)|0xfff0 if c&1 else (old&0xf000)|0xfff)
            elif bits==16:struct.pack_into('<H',data,at+c*2,0xffff)
            else:struct.pack_into('<I',data,at+c*4,0x0fffffff)
    truth={};names={};raw={};slots=[]
    def record(name,expected,cluster,parent='',long_name=None,flags=0,location=None):
        payload=('known CP932 payload '+str(bits)+' '+str(cluster)+'\n').encode()
        slot=sfn(name,cluster,len(payload));slot[12]=flags
        if location is None:
            if long_name:slots.extend(lfn(long_name,name))
            at=root+len(slots)*32;slots.append(slot)
        else:at=location;data[at:at+32]=slot
        key=f'fat{bits}-{at}';truth[key]=payload;names[key]=parent+expected
        raw[key]=None if long_name else bytes(slot[:11]).hex().upper()
        data[address(cluster):address(cluster)+len(payload)]=payload
    record('日本語'.encode('cp932').ljust(8,b' ')+b'TXT','?{FA967B}語.TXT',1000)
    record(b'A'+ 'ｶﾀｶﾅ'.encode('cp932')+b'   TXT','?{B6}ﾀｶﾅ.TXT',1001)
    record(b'A\x81\x7f     TXT','?{817F}.TXT',1002)
    record(b'LOWER   TXT','?ower.txt',1003,flags=0x18)
    record(b'JAPANE~1TXT','長い日本語名.txt',1004,long_name='長い日本語名.txt')
    for c,name,expected,payload_cluster in [(folder,'日本'.encode('cp932').ljust(8,b' ')+b'   ','日本',1100),
                                            (50,b'\x05\x5c      '+b'   ','藹',1101),
                                            (51,bytes.fromhex('fa408790').ljust(8,b' ')+b'   ','ⅰ≒',1102)]:
        end_chain(c);data[address(c):address(c)+512]=bytes(512)
        slots.append(sfn(name,c,0,deleted=False,directory=True))
        record(b'CHILD   TXT','?HILD.TXT',payload_cluster,parent=expected+'/',location=address(c))
    check(len(slots)<16,'fixture root overflow')
    for i,slot in enumerate(slots):data[root+i*32:root+(i+1)*32]=slot
    path.write_bytes(data)
    return truth,names,raw


def verify_names(rows,names,raw):
    by_id={r['id']:r for r in rows};check(set(by_id)==set(names),'candidate count')
    for key,expected in names.items():
        row=by_id[key];check(row['original_path']==expected,(expected,row['original_path']))
        check(row['supported'] and not row['path_verified'],'uncertain deleted name became verified')
        if raw[key]:check(any('raw_hex='+raw[key] in w and 'encoding=cp932' in w for w in row['warnings']),'raw short-name bytes absent')
        else:check(not any('encoding=cp932' in w for w in row['warnings']),'LFN decoded as OEM')
    check(any('CP932_LEADING_BOUNDARY_AMBIGUOUS:' in w for r in rows for w in r['warnings']),'ambiguity not reported')
    check(any('CP932_INVALID_BYTES:' in w for r in rows for w in r['warnings']),'invalid bytes silently replaced')


def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-cp932-') as temporary:
        root=Path(temporary)
        for bits in [12,16,32]:
            source=root/f'fat{bits}.raw';truth,names,raw=make(source,bits);before=file_hash(source)
            default=run(binary,'scan','--source',source,'--work-dir',root/f'{bits}-default','--json')
            old=json.loads(Path(default['session']).read_text(encoding='utf-8'))
            check(any('\ufffd' in c['original_path'] for c in old['candidates']),'default unexpectedly inferred CP932')
            del old['search']['fat_short_name_encoding'];Path(default['session']).write_text(json.dumps(old),encoding='utf-8')
            restored=run(binary,'recover','--session',default['session'],'--all','--destination',root/f'{bits}-old-out','--json')
            for row in restored['results']:check(Path(row['path']).read_bytes()==truth[row['candidate_id']],'old session compatibility');exact+=1
            checks.append(f'fat{bits}_default_and_missing_setting_stay_compatible')
            summaries={}
            for mode,extra in [('json',[]),('index',['--indexed'])]:
                summary=run(binary,'scan','--source',source,'--work-dir',root/f'{bits}-{mode}','--fat-short-name-encoding','cp932',*extra,'--json');summaries[mode]=summary
                rows=pages(binary,summary['session']) if extra else json.loads(Path(summary['session']).read_text(encoding='utf-8'))['candidates']
                verify_names(rows,names,raw)
                saved=run(binary,'report','--session',summary['session'],'--json')
                header=saved['session'] if extra else json.loads(Path(summary['session']).read_text(encoding='utf-8'))
                check(header['schema_version']==10 and header['search']['fat_short_name_encoding']=='cp932','encoding needs feature-versioned session')
                schema=json.loads((Path(__file__).resolve().parent.parent/'schemas/v1.json').read_text())
                jsonschema.validate(header,{'$schema':schema['$schema'],'$ref':'#/$defs/Session','$defs':schema['$defs']})
                restored=run(binary,'recover','--session',summary['session'],'--all','--destination',root/f'{bits}-{mode}-out','--json')
                for row in restored['results']:
                    key=row['candidate_id'];check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[key],'recovery bytes');exact+=1
                    if raw[key]:check(any('raw_hex='+raw[key] in w for w in row['warnings']),'recovery omitted raw name')
                if mode=='json':expected=rows;report=Path(restored['report'])
                else:check(rows==expected,'index changed decoding or provenance')
            checks.extend([f'fat{bits}_cli_index_names_and_bytes',f'fat{bits}_deleted_lead_ambiguity_and_invalid_bytes',f'fat{bits}_lfn_and_escaped_e5_parent_names'])
            original=json.loads(report.read_text(encoding='utf-8'))
            for kind in ['html','csv']:
                path=root/f'{bits}-report.{kind}';run(binary,'export-report','--input',report,'--output',path,'--format',kind,'--json')
                if kind=='html':parsed=Document();parsed.feed(path.read_text(encoding='utf-8'));values=[json.loads(p) for p in parsed.pre[:-1]]
                else:values=[json.loads(row['result_json']) for row in csv.DictReader(io.StringIO(path.read_text(encoding='utf-8-sig')))]
                check(values==original['results'],'report changed code-page evidence')
            checks.append(f'fat{bits}_html_csv_preserve_raw_names')
            archive=root/f'{bits}.zip';result=run(binary,'recover','--session',summaries['index']['session'],'--all','--archive',archive,'--json')
            with zipfile.ZipFile(archive) as z:
                record=json.loads(z.read('rescue-report.json'));check(z.testzip() is None,'zip CRC')
                for row in record['results']:check(z.read(row['archive_entry'])==truth[row['candidate_id']],'ZIP bytes');exact+=1
            checks.append(f'fat{bits}_zip_bytes')
            preview=run(binary,'preview','--session',summaries['index']['session'],'--id',next(iter(truth)),'--destination',root/f'{bits}-preview','--json')
            check(Path(preview['path']).read_bytes()==truth[next(iter(truth))],'preview revalidation used wrong encoding')
            checks.append(f'fat{bits}_preview_revalidates_saved_code_page')
            for scheme,extra,sector in [('mbr',[],512),('gpt',['--indexed'],4096)]:
                disk=root/f'{bits}-{scheme}.raw';disk_image(disk,[source],sector,scheme)
                scanned=run(binary,'scan','--source',disk,'--work-dir',root/f'{bits}-{scheme}','--fat-short-name-encoding','cp932',*extra,'--json')
                disk_rows=pages(binary,scanned['session']) if extra else json.loads(Path(scanned['session']).read_text(encoding='utf-8'))['candidates']
                unprefixed=[dict(row,id=row['id'].split(':',1)[1],original_path=row['original_path'].split('/',1)[1]) for row in disk_rows]
                verify_names(unprefixed,names,raw)
                recovered=run(binary,'recover','--session',scanned['session'],'--all','--destination',root/f'{bits}-{scheme}-out','--json')
                for row in recovered['results']:check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id'].split(':',1)[1]],'partitioned recovery');exact+=1
            checks.append(f'fat{bits}_mbr_json_gpt_index_names_and_bytes')
            feature=json.loads(Path(summaries['json']['session']).read_text(encoding='utf-8'));feature['schema_version']=3
            downgraded=root/f'{bits}-downgraded.json';downgraded.write_text(json.dumps(feature),encoding='utf-8')
            run(binary,'recover','--session',downgraded,'--all','--destination',root/f'{bits}-downgraded-out','--json',expected=4)
            check(not (root/f'{bits}-downgraded-out').exists(),'downgraded encoding wrote files')
            checks.append(f'fat{bits}_feature_schema_downgrade_refused')
            client=Client(binary,source,root/f'{bits}-mcp-work',root/f'{bits}-mcp-out')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                client.tool('start_scan',{'source':str(source),'fat_short_name_encoding':'guess','request_id':'bad'},error=True)
                job=client.wait(client.tool('start_scan',{'source':str(source),'indexed':True,'fat_short_name_encoding':'cp932','request_id':'scan'})['job_id']);sid=job['result']['session_id']
                rows=candidate_evidence(client.tool('list_candidates',{'session_id':sid,'limit':100}));verify_names(rows,names,raw)
                job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(root/f'{bits}-mcp-out'),'request_id':'restore'})['job_id'])
                for row in job['result']['results']:check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id']],'MCP bytes');exact+=1
            finally:client.close()
            checks.append(f'fat{bits}_mcp_names_bytes_and_option_validation')
            check(file_hash(source)==before,'source changed')
        # Interrupt a real indexed traversal. A mismatched saved option must fail,
        # and the untouched checkpoint must resume with exactly the same names.
        source=root/'large.raw';large(source,'fat32',fat32)
        with source.open('r+b') as f:
            f.seek((32+2*513)*512);f.write(bytes.fromhex('e5fa967b8cea2020545854'))
        before=file_hash(source)
        full=run(binary,'scan','--source',source,'--fat-short-name-encoding','cp932','--work-dir',root/'full','--json')
        expected=json.loads(Path(full['session']).read_text(encoding='utf-8'))['candidates']
        child=subprocess.Popen([str(binary),'scan','--indexed','--source',str(source),'--fat-short-name-encoding','cp932','--work-dir',str(root/'crash'),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        try:
            while True:
                line=child.stderr.readline();check(bool(line),'no checkpoint progress');event=json.loads(line)
                if event['progress']['scanned_records']>=128:break
            path=Path(event['index']);child.kill();child.communicate(timeout=20)
        finally:
            if child.poll() is None:child.kill();child.wait(timeout=20)
        with contextlib.closing(sqlite3.connect(path)) as db:
            saved=db.execute('SELECT engine_state FROM checkpoint').fetchone()[0];state=json.loads(saved)
            check(state['options']['fat_short_name_encoding']=='cp932','checkpoint omitted code page')
            state['options']['fat_short_name_encoding']='ascii';db.execute('UPDATE checkpoint SET engine_state=?',(json.dumps(state),));db.commit()
        run(binary,'resume','--session',path,'--json',expected=2)
        with contextlib.closing(sqlite3.connect(path)) as db:db.execute('UPDATE checkpoint SET engine_state=?',(saved,));db.commit()
        check(run(binary,'resume','--session',path,'--json')['index']['state']=='completed','resume failed')
        check(pages(binary,path)==expected,'resumed names/provenance differ');check(file_hash(source)==before,'large source changed')
        checks.extend(['checkpoint_retains_code_page','mismatched_checkpoint_refused','process_kill_resume_preserves_names_and_raw_bytes'])
        run(binary,'scan','--source',source,'--work-dir',root/'invalid-mode','--mode','carve','--fat-short-name-encoding','cp932','--json',expected=2)
        checks.append('code_page_option_rejected_for_carving')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path)
    args=parser.parse_args();main(args.binary.resolve(),args.output)
