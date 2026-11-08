"""FAT12/16 fixed roots, packed FAT boundaries, ownership and all frontends.

Fixtures are ordinary files. Byte truth is generated independently of the Rust
parser. Existing FAT32/exFAT adversarial suites still run unchanged.
"""
import argparse,hashlib,json,struct,subprocess,tempfile
from pathlib import Path
from e2e import Client,check,run
from e2e_index_fat import pages
from make_fat_fixture import sfn,lfn,put,file_hash


def make(path,bits):
    sector=512;clusters=2048 if bits==12 else 8192;entries=512
    fat_bytes=((clusters+2)*3+1)//2 if bits==12 else (clusters+2)*2
    fat_sectors=(fat_bytes+511)//512;root_at=(1+2*fat_sectors)*sector
    heap=root_at+entries*32;total=heap+clusters*sector
    volume=bytearray(total);boot=bytearray(sector)
    boot[:3]=b'\xeb\x3c\x90';boot[3:11]=b'RESCUET '
    put(boot,11,'H',sector);boot[13]=1;put(boot,14,'H',1);boot[16]=2
    put(boot,17,'H',entries);put(boot,19,'H',total//sector);boot[21]=0xf8
    put(boot,22,'H',fat_sectors);boot[38]=0x29;boot[54:62]=b'NOTALABL'
    boot[510:512]=b'\x55\xaa';volume[:512]=boot;fat=bytearray(fat_sectors*sector)
    def setfat(cluster,value):
        if bits==16:put(fat,cluster*2,'H',value)
        else:
            at=cluster*3//2;old=struct.unpack_from('<H',fat,at)[0]
            put(fat,at,'H',(old&0xf)|value<<4 if cluster&1 else (old&0xf000)|value)
    end=(1<<bits)-1
    for c,v in [(0,end-7),(1,end),(341,800),(800,342),(342,end),(7,end),(999,end),(120,121),(121,120)]:setfat(c,v)
    address=lambda c:heap+(c-2)*sector
    truth={};unsupported=[];slots=[]
    def add(name,first,data,long_name=None,supported=True,high_word=0):
        if long_name:slots.extend(lfn(long_name,name))
        offset=root_at+len(slots)*32;slot=sfn(name,first,len(data));put(slot,20,'H',high_word);slots.append(slot)
        key=f'fat{bits}-{offset}'
        if supported:truth[key]=data
        else:unsupported.append(key)
        return key
    small=add(b'SMALL   TXT',30,b'known FAT first cluster\n',high_word=0x1234)
    fragmented=b'A'*512+b'B'*512+b'C'*276
    fragment=add(b'FRAGMENTBIN',341,fragmented)
    lost=add(b'LOST    BIN',110,b'?'*900,supported=False)
    add(b'LOOP    BIN',120,b'?'*1024,supported=False)
    add(b'REUSED  TXT',999,b'live data',supported=False)
    empty=add(b'EMPTY   TXT',0,b'')
    name='日本語の長いファイル名.txt';japanese=add(b'JAPANE~1TXT',130,'日本語ファイル\n'.encode(),long_name=name)
    slots.append(sfn(b'LIVE    TXT',999,9,deleted=False))
    slots.append(sfn(b'FOLDER     ',7,0,deleted=False,directory=True))
    # Exercise an indexed restart point inside a fixed root beyond one sector.
    for i in range(300):slots.append(sfn(f'F{i:07d}TXT'.encode(),0,0,deleted=False))
    late=add(b'LATE    TXT',131,b'fixed root tail\n')
    for i,slot in enumerate(slots):volume[root_at+i*32:root_at+(i+1)*32]=slot
    child=b'nested fixed-root recovery';truth[f'fat{bits}-{address(7)}']=child
    volume[address(7):address(7)+32]=sfn(b'CHILD   TXT',8,len(child))
    for c,data in [(30,truth[small]),(341,b'A'*512),(800,b'B'*512),(342,b'C'*276),(130,truth[japanese]),(131,truth[late]),(8,child),(999,b'live data')]:volume[address(c):address(c)+len(data)]=data
    for copy in range(2):volume[(1+copy*fat_sectors)*sector:(1+(copy+1)*fat_sectors)*sector]=fat
    path.write_bytes(volume)
    return truth,unsupported,{'root':root_at,'fat_sectors':fat_sectors,'fragment':fragment,'lost':lost,'name':name,'japanese':japanese,'heap':heap}


def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-classic-fat-') as temporary:
        root=Path(temporary)
        for bits in [12,16]:
            source=root/f'fat{bits}.raw';truth,unsupported,meta=make(source,bits);before=file_hash(source)
            old=run(binary,'scan','--source',source,'--work-dir',root/f'old-{bits}','--json')
            indexed=run(binary,'scan','--indexed','--source',source,'--work-dir',root/f'index-{bits}','--json')
            check(old['filesystem']==f'fat{bits}' and run(binary,'report','--session',indexed['session'],'--json')['session']['filesystem']==f'fat{bits}','BPB detection independent of label')
            expected=json.loads(Path(old['session']).read_text())['candidates']
            check(pages(binary,indexed['session'])==expected,'fixed-root indexed evidence mismatch')
            by_id={c['id']:c for c in expected}
            check(set(by_id)==set(truth)|set(unsupported),'FAT root/subdirectory candidates')
            check({k for k,c in by_id.items() if c['supported']}==set(truth),'unknown or reused chain accepted')
            check(by_id[meta['japanese']]['name']==meta['name'],'LFN Unicode name')
            for mode,summary in [('legacy',old),('index',indexed)]:
                result=run(binary,'recover','--session',summary['session'],'--all','--destination',root/f'{bits}-{mode}-out','--json')
                for row in result['results']:
                    check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id']],row);exact+=1
                check(len(result['results'])==len(truth),'all selection contains unknown candidates')
            client=Client(binary,source,root/f'{bits}-mcp-work',root/f'{bits}-mcp-out')
            try:
                client.rpc('initialize',{'protocolVersion':'2025-11-25'})
                job=client.wait(client.tool('start_scan',{'source':str(source),'indexed':True,'request_id':'classic-fat'})['job_id'])
                sid=job['result']['session_id']
                restored=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(root/f'{bits}-mcp-out'),'request_id':'classic-recover'})['job_id'])
                for row in restored['result']['results']:
                    check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id']],row);exact+=1
            finally:client.close()
            check(file_hash(source)==before,'source modified')
            checks += [f'fat{bits}_fixed_root_lfn_subdirectory_and_packed_boundary',f'fat{bits}_legacy_index_mcp_bytes',f'fat{bits}_cleared_loop_live_overlap_refused']
            # Oversized table/root, truncated volume and incompatible geometry
            # must fail before emitting a plausible successful empty session.
            for label,change in [('short_fat',lambda b:put(b,22,'H',1)),('no_root',lambda b:put(b,17,'H',0)),('bad_cluster',lambda b:b.__setitem__(13,3)),('conflicting_totals',lambda b:put(b,32,'I',999999))]:
                bad=root/f'{bits}-{label}.raw';data=bytearray(source.read_bytes());change(data);bad.write_bytes(data)
                process=subprocess.run([str(binary),'scan','--source',str(bad),'--work-dir',str(root/f'{bits}-{label}-work'),'--json'],capture_output=True)
                check(process.returncode!=0,(label,process.stdout));checks.append(f'fat{bits}_{label}_refused')
            from e2e_disks import disk_image
            for scheme,sector in [('mbr',512),('gpt',4096)]:
                disk=root/f'{bits}-{scheme}.raw';disk_image(disk,[source],sector,scheme)
                summary=run(binary,'scan','--indexed','--source',disk,'--work-dir',root/f'{bits}-{scheme}-work','--json')
                result=run(binary,'recover','--session',summary['session'],'--all','--destination',root/f'{bits}-{scheme}-out','--json')
                for row in result['results']:
                    check(row['status']=='completed' and Path(row['path']).read_bytes()==truth[row['candidate_id'].split(':',1)[1]],row);exact+=1
                check(len(result['results'])==len(truth),'partition FAT selection');checks.append(f'fat{bits}_{scheme}_bounded_volume')
    report={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(report,ensure_ascii=False));return report


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
