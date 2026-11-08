"""Regressions for GitHub issues 1-3: raw batches, FAT provenance, file types."""
import argparse,hashlib,json,sqlite3,struct,subprocess,tempfile
from pathlib import Path
from e2e import Client,check,run
from make_fixture import build,record,filename,resident,MFT_LCN,SECTOR,RECORD
from make_fat_fixture import fat32,file_hash
from e2e_index_fat import large,pages


def main(binary,output=None):
    cases=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-fixes-') as temporary:
        root=Path(temporary);source=root/'names.raw';truth=build(source)
        long_names={30:'a'*236+'.txt',31:'長'*76+'.txt'};payloads={20:bytes.fromhex(truth['mft-20-7']['hex']),30:b'long ASCII original',31:b'long Unicode original'}
        with source.open('r+b') as f:
            for number,name in long_names.items():f.seek(MFT_LCN*SECTOR+number*RECORD);f.write(record(number,[filename(name),resident(0x80,payloads[number])]))
            f.seek(MFT_LCN*SECTOR+32*RECORD);f.write(record(32,[filename('original:colon.txt'),resident(0x80,b'base'),resident(0x80,b'stream',name='stream.dat')]))
        before=file_hash(source);scan=run(binary,'scan','--indexed','--source',source,'--work-dir',root/'index','--json');session=scan['session']
        rows=pages(binary,session)
        with sqlite3.connect(session) as db:kinds={row[0]:row[1] for row in db.execute('SELECT id,type_key FROM candidate')}
        check(kinds['mft-24-7']=='TXT','literal colon type corrupted')
        stream=next(c for c in rows if c['stream_name']=='stream.dat');check(kinds[stream['id']]=='TXT','explicit ADS suffix not stripped')
        cases+=['literal_colon_and_last_component_type','explicit_ads_uses_base_extension']
        args=[x for n in [20,30,31] for x in ['--id',f'mft-{n}-7']]
        result=run(binary,'export-raw','--session',session,*args,'--destination',root/'raw','--json')
        check(result['exported']==3 and result['not_exportable']==0,'long names failed batch')
        for row in result['exports']:
            n=int(row['candidate_id'].split('-')[1]);path=Path(row['bin_path']);check(path.read_bytes()==payloads[n],'raw bytes');exact+=1
            check(len(path.name.encode())<=255 and len(Path(row['map_path']).name.encode())<=255,'suffix exceeded component limit')
            check(path.name.startswith(row['candidate_id']+'.') and path.name.endswith('.txt.rescue-raw.bin'),'ID/extension lost')
            saved=json.loads(Path(row['map_path']).read_text(encoding='utf-8'))
            if n in long_names:check(saved['naming']['original_name']==long_names[n],'full original name lost')
        cases+=['suffix_included_in_ascii_utf8_filename_budget','original_name_preserved_in_raw_map']
        mixed=root/'mixed';mixed.mkdir();long_row=next(r for r in result['exports'] if r['candidate_id']=='mft-30-7');(mixed/Path(long_row['bin_path']).name).mkdir()
        failed=run(binary,'export-raw','--session',session,*args,'--destination',mixed,'--json',expected=5)
        check(failed['exported']==2 and failed['not_exportable']==1 and len(failed['exports'])==3,'partial batch results lost')
        for row in failed['exports']:
            n=int(row['candidate_id'].split('-')[1])
            if n==30:check(row['status']=='not_exportable' and row['error'] and row['bin_path'] is None,'failure misreported')
            else:check(Path(row['bin_path']).read_bytes()==payloads[n],'batch did not continue');exact+=1
        cases.append('per_file_failure_preserves_earlier_and_later_exports')
        partial=root/'map-blocked';partial.mkdir();small=next(r for r in result['exports'] if r['candidate_id']=='mft-20-7');blocked=partial/Path(small['map_path']).name;blocked.write_bytes(b'keep existing map')
        failed=run(binary,'export-raw','--session',session,'--id','mft-20-7','--id','mft-31-7','--destination',partial,'--json',expected=5)
        check(failed['not_exportable']==1 and failed['exported']==1,'map failure ended batch')
        for row in failed['exports']:
            n=int(row['candidate_id'].split('-')[1]);check(Path(row['bin_path']).read_bytes()==payloads[n],'written bytes missing from report');exact+=1
            if n==20:check(row['status']=='not_exportable' and row['map_path'] is None and row['error'] and row['sha256']==file_hash(Path(row['bin_path'])),'partial pair hidden')
        check(blocked.read_bytes()==b'keep existing map','map overwritten');cases.append('map_failure_reports_owned_bytes_and_preserves_existing_map')
        client=Client(binary,source,root/'mcp-work',root/'mcp-out')
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'})
            job=client.wait(client.tool('start_scan',{'source':str(source),'indexed':True,'request_id':'scan'})['job_id'])
            recovered=client.wait(client.tool('export_unrecovered',{'session_id':job['result']['session_id'],'candidate_ids':['mft-30-7','mft-31-7'],'destination':str(root/'mcp-out'),'request_id':'raw'})['job_id'])
            for row in recovered['result']['exports']:
                n=int(row['candidate_id'].split('-')[1]);check(Path(row['bin_path']).read_bytes()==payloads[n],'MCP long raw');exact+=1
            check(len(recovered['result']['exports'])==2,'MCP result count')
        finally:client.close()
        cases.append('mcp_long_name_exports_byte_exact');check(file_hash(source)==before,'source modified')
        # Force a real process interruption after a persisted root-mirror decision.
        source=root/'mirror.raw';large(source,'fat32',fat32)
        with source.open('r+b') as f:
            boot=f.read(512);reserved=struct.unpack_from('<H',boot,14)[0];fat_sectors=struct.unpack_from('<I',boot,36)[0]
            f.seek((reserved+fat_sectors)*512+2*4);f.write(struct.pack('<I',0x0ffffff7))
        before=file_hash(source);old=run(binary,'scan','--source',source,'--work-dir',root/'mirror-old','--json')
        expected=run(binary,'report','--session',old['session'],'--json')['warnings'];warnings=[w for w in expected if 'Damaged FAT mirror:' in w];check(warnings,'baseline lacks provenance')
        process=subprocess.Popen([str(binary),'scan','--indexed','--source',str(source),'--work-dir',str(root/'mirror-index'),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=process.stderr.readline();check(bool(line),('early mirror scan exit',process.poll()));event=json.loads(line);index=Path(event['index'])
            if event['progress']['scanned_records']>=128:break
        process.kill();process.communicate(timeout=20)
        with sqlite3.connect(index) as db:checkpoint=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0])
        check(checkpoint['fat']['copy_choices'] and 'FAT copy 0' in checkpoint['fat']['copy_choices'][0],'checkpoint lost mirror choice')
        run(binary,'resume','--session',index,'--json')
        header=run(binary,'report','--session',index,'--json')['session'];check([w for w in header['warnings'] if 'Damaged FAT mirror:' in w]==warnings,'resumed warning differs')
        check(file_hash(source)==before,'mirror source modified');cases+=['indexed_mirror_decision_in_checkpoint','crash_resume_preserves_count_first_target_and_copy']
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(cases),'cases':cases,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
