"""Independent array encoder vs real CLI/MCP. Only newly-created regular images.

P/Q oracle uses polynomial multiplication; product uses logarithm tables. No
controller, physical fault or blanket vendor-compatibility claim is made.
"""
import argparse,copy,hashlib,itertools,json,os,platform,subprocess,tempfile,threading
from pathlib import Path
from queue import Queue
from e2e import Client,run,check
from e2e_imaging import mark_gap
from make_fixture import build
from make_fat_fixture import fat32,exfat

def gf(a,b):
    value=0
    for i in range(8):
        if b&(1<<i):value^=a<<i
    for i in range(14,7,-1):
        if value&(1<<i):value^=0x11d<<(i-8)
    return value

def encode(root,logical,level,n=4,chunk=4096,layout='fixed',offset=4096,sector=512):
    root.mkdir(parents=True,exist_ok=True)
    data=n if level in ['raid0','jbod'] else 1 if level=='raid1' else n//2 if level=='raid10' else n-(2 if level=='raid6' else 1)
    size=((len(logical)+data*chunk-1)//(data*chunk))*data*chunk;logical=logical.ljust(size,b'\0')
    output=[bytearray() for _ in range(n)]
    if level=='jbod':
        for i in range(n):output[i]+=logical[i*(size//n):(i+1)*(size//n)]
    else:
        coeff=[1]
        for _ in range(data-1):coeff.append(gf(coeff[-1],2))
        translations=[bytes(gf(v,c) for v in range(256)) for c in coeff]
        for stripe,start in enumerate(range(0,size,data*chunk)):
            blocks=[logical[start+i*chunk:start+(i+1)*chunk] for i in range(data)]
            if level=='raid0':encoded=blocks
            elif level=='raid1':encoded=blocks*n
            elif level=='raid10':encoded=[b for b in blocks for _ in range(2)]
            else:
                encoded=[None]*n
                if level=='raid6':p,q=n-2,n-1;order=list(range(data))
                else:
                    p=(n-1-stripe%n) if layout.startswith('left') else stripe%n;q=None
                    order=[i for i in range(n) if i!=p] if layout.endswith('asymmetric') else [(p+i)%n for i in range(1,n)]
                pv=0;qv=0
                for j,b in enumerate(blocks):
                    encoded[order[j]]=b;pv^=int.from_bytes(b,'little');qv^=int.from_bytes(b.translate(translations[j]),'little')
                encoded[p]=pv.to_bytes(chunk,'little')
                if q is not None:encoded[q]=qv.to_bytes(chunk,'little')
            for i,b in enumerate(encoded):output[i]+=b
    members=[]
    for i,b in enumerate(output):
        path=root/f'member-{i}.raw';path.write_bytes(bytes([0xa0+i])*offset+b+b'unused trailing bytes')
        members.append({'path':str(path.resolve()),'offset':offset,'length':len(b)})
    return {'schema_version':1,'level':level,'layout':layout,'sector_size':sector,'stripe_bytes':chunk,'members':members},logical

def save(root,config,name='array'):
    p=root/(name+'.rescue-raid.json');p.write_text(json.dumps(config),encoding='utf-8');return p

def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()

class MultiClient(Client):
    def __init__(self,binary,sources,work,out):
        self.process=subprocess.Popen([str(binary),'mcp','--stdio',*[x for p in sources for x in ['--allow-source',str(p)]],'--work-dir',str(work),'--allow-output-root',str(out)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8',bufsize=1)
        self.queue=Queue();self.next_id=0;threading.Thread(target=self.reader,daemon=True).start()

def main(binary,output=None):
    checks=[];exact_arrays=0;exact_files=0
    check(gf(1,1)^gf(2,2)^gf(3,4)==9,'fixed independent Q vector')
    with tempfile.TemporaryDirectory(prefix='rescue-raid-') as tmp:
        root=Path(tmp);logical=bytes((i*31+i//127)%256 for i in range(96*1024))
        profiles=[('jbod','fixed'),('raid0','fixed'),('raid1','fixed'),('raid10','fixed')]+[('raid5',x) for x in ['left_symmetric','right_symmetric','left_asymmetric','right_asymmetric']]+[('raid6','pq_last')]
        for index,(level,layout) in enumerate(profiles):
            case=root/f'p{index}';config,expected=encode(case,logical,level,layout=layout,chunk=512 if index%2==0 else 4096)
            before={m['path']:digest(Path(m['path'])) for m in config['members']}
            missing_sets=[()]
            if level in ['raid1','raid5']:missing_sets += [(i,) for i in range(4)]
            if level=='raid10':missing_sets += [(0,2),(1,3),(0,3),(1,2)]
            if level=='raid6':missing_sets+= [(i,) for i in range(4)]+list(itertools.combinations(range(4),2))
            for trial,missing in enumerate(missing_sets):
                conf=copy.deepcopy(config)
                for i in missing:conf['members'][i]['path']=None
                path=save(case,conf,f'input{trial}');sealed=case/f'sealed{trial}.rescue-raid.json'
                info=run(binary,'raid','--config',path,'--output',sealed,'--json');check(info['unavailable_sample_bytes']==0,'expected recoverable sample '+str((level,missing,info)))
                acquired=run(binary,'image','--source',sealed,'--destination',case/f'acquired{trial}','--block-size',4096,'--retries',0,'--json',timeout=90)
                check(Path(acquired['image']).read_bytes()==expected,'logical bytes '+str((level,layout,missing)));exact_arrays+=1
            check(all(digest(Path(p))==h for p,h in before.items()),'member modified');checks.append(level+'/'+layout+' all supported missing positions')
        # Every required layout feeds the same filesystem recovery engine.
        source=root/'ntfs.raw';truth=build(source)
        for index,(level,layout) in enumerate(profiles):
            case=root/f'fs{index}';config,_=encode(case,source.read_bytes(),level,layout=layout)
            if level=='raid6':config['members'][0]['path']=None;config['members'][1]['path']=None
            if level=='raid5':config['members'][0]['path']=None
            path=save(case,config);scan=run(binary,'scan','--source',path,'--work-dir',case/'work','--json',timeout=90)
            session=json.loads(Path(scan['session']).read_text(encoding='utf-8'));check(session['schema_version']==9 and session['raid']['config']['level']==level,'provenance')
            result=run(binary,'recover','--session',scan['session'],'--all','--destination',case/'out','--json',timeout=90)['results']
            check({r['candidate_id'] for r in result}==set(truth),'array filesystem enumeration')
            for item in result:check(item['status']=='completed' and digest(Path(item['path']))==truth[item['candidate_id']]['sha256'],'array filesystem bytes')
            for item in result:
                trace=item['raid_reads'];check(trace['ranges'] and trace['omitted_ranges']==0,'array read evidence missing')
                if level=='raid6':check(all(r['used_members']==[2,3] and r['missing_members']==[0,1] and r['parity_members']==[2,3] for r in trace['ranges']),'P/Q member evidence differs')
            exact_files+=len(result)
        checks.append('all nine profiles NTFS recovery and schema9 provenance')
        for label,make,level,layout in [('fat32',fat32,'raid5','right_asymmetric'),('exfat',exfat,'raid6','pq_last')]:
            case=root/label;case.mkdir();src=case/'volume.raw';truth_fs,_,_=make(src);config,_=encode(case/'members',src.read_bytes(),level,layout=layout,chunk=65536)
            config['members'][0]['path']=None
            if level=='raid6':config['members'][1]['path']=None
            scan=run(binary,'scan','--source',save(case,config),'--work-dir',case/'work','--json',timeout=90)
            recovered=run(binary,'recover','--session',scan['session'],'--all','--destination',case/'out','--json',timeout=90)['results'];check({r['candidate_id'] for r in recovered}==set(truth_fs),'FAT family RAID candidates')
            for item in recovered:check(item['status']=='completed' and Path(item['path']).read_bytes()==truth_fs[item['candidate_id']],'FAT family RAID bytes')
            exact_files+=len(recovered)
        checks.append('FAT32 and exFAT filesystem recovery from missing-member arrays')
        # The allowed number of missing slots is never interpreted as zero-filled data.
        for level,layout,missing in [('jbod','fixed',[1]),('raid0','fixed',[1]),('raid1','fixed',[0,1,2,3]),('raid10','fixed',[0,1]),('raid5','left_symmetric',[0,1]),('raid6','pq_last',[0,1,2])]:
            case=root/('missing-'+level);config,expected=encode(case,logical,level,layout=layout)
            for i in missing:config['members'][i]['path']=None
            path=save(case,config);info=run(binary,'raid','--config',path,'--json');check(info['unavailable_sample_bytes']>0,'missing slots fabricated '+level)
            acquired=run(binary,'image','--source',path,'--destination',case/'partial','--block-size',4096,'--retries',1,'--json',expected=5,timeout=90)
            coverage=run(binary,'read-map','--image',acquired['image'],'--limit',1000,'--json');check(coverage['failed_bytes']>0,'missing ranges lost in acquired map '+level)
        checks.append('all six levels retain unrecoverable gaps in logical acquisition maps')
        # Full 4 KiB sectors and explicit offsets preserve bytes too.
        case=root/'sector4096';config,expected=encode(case,logical,'raid6',layout='pq_last',chunk=65536,sector=4096,offset=8192)
        config['members'][0]['path']=None;config['members'][3]['path']=None
        path=save(case,config);acquired=run(binary,'image','--source',path,'--destination',case/'acquired','--block-size',65536,'--json');check(Path(acquired['image']).read_bytes()==expected,'4K geometry');exact_arrays+=1;checks.append('4096-byte sector and offset P/Q reconstruction')
        # Alternative search validates internal filesystem structures and respects caps.
        case=root/'search';config,_=encode(case,source.read_bytes(),'raid5',layout='left_symmetric',chunk=4096)
        path=save(case,config)
        found=run(binary,'raid','--config',path,'--search','--stripe-sizes','4096,8192','--max-milliseconds',30000,'--json',timeout=90)
        check(any(c['inspection']['evidence']['config']['stripe_bytes']==4096 and c['inspection']['evidence']['config']['layout']=='left_symmetric' and len(c['filesystem_evidence'])>=3 for c in found['candidates']),'correct structural alternative absent '+str(found))
        limited=run(binary,'raid','--config',path,'--search','--reorder','--max-configurations',1,'--json');check(limited['tested_configurations']==1 and limited['truncated'],'search cap not explicit');checks.append('filesystem-validated alternatives and explicit bounded search')
        # Do not trust provenance edited independently of the actual source configuration.
        scan=run(binary,'scan','--source',path,'--work-dir',case/'work','--json');session=json.loads(Path(scan['session']).read_text(encoding='utf-8'));session['raid']['identity_sha256']='0'*64
        edited=case/'edited.json';edited.write_text(json.dumps(session),encoding='utf-8')
        p=subprocess.run([str(binary),'recover','--session',str(edited),'--all','--destination',str(case/'edited-out'),'--json'],capture_output=True,text=True);check(p.returncode and 'SOURCE_CHANGED' in p.stderr,'forged array evidence trusted');checks.append('tampered session array evidence rejected')
        # Two independently readable mirror copies that disagree must not be chosen automatically.
        case=root/'inconsistent';config,_=encode(case,logical,'raid1');member=Path(config['members'][0]['path']);data=bytearray(member.read_bytes());data[4096+7]^=0xff;member.write_bytes(data)
        info=run(binary,'raid','--config',save(case,config),'--json');check(info['unavailable_sample_bytes']>0,'mismatched mirror silently selected');checks.append('mirror mismatch unavailable')
        for level,layout in [('raid5','left_symmetric'),('raid6','pq_last')]:
            case=root/('bad-'+level);config,_=encode(case,logical,level,layout=layout);m=Path(config['members'][-1]['path']);data=bytearray(m.read_bytes());data[4100]^=1;m.write_bytes(data)
            check(run(binary,'raid','--config',save(case,config),'--json')['unavailable_sample_bytes']>0,'parity mismatch ignored');checks.append(level+' parity mismatch unavailable')
        # A read-map gap is a missing member range and can be reconstructed from another mirror.
        case=root/'mapped';config,expected=encode(case,logical,'raid1',n=2)
        acquired=run(binary,'image','--source',config['members'][0]['path'],'--destination',case/'member-image','--block-size',4096,'--json');mapped=Path(acquired['image']);mark_gap(mapped,4096);config['members'][0]['path']=str(mapped)
        path=save(case,config);acquired=run(binary,'image','--source',path,'--destination',case/'logical','--block-size',4096,'--json');check(Path(acquired['image']).read_bytes()==expected,'map gap mirror reconstruction');checks.append('member read-map gap reconstruction')
        # Manifest identity binds order/offset/size, and sealed members reject subsequent changes.
        sealed=case/'sealed.rescue-raid.json';run(binary,'raid','--config',path,'--output',sealed,'--json');m=Path(config['members'][1]['path']);m.write_bytes(m.read_bytes()+b'x')
        p=subprocess.run([str(binary),'inspect','--source',str(sealed),'--json'],capture_output=True,text=True);check(p.returncode and 'SOURCE_CHANGED' in p.stderr,'sealed changed member accepted');checks.append('sealed member change rejected')
        # No hard-link aliases can count as independent redundant members.
        alias=case/'alias.raw';os.link(m,alias);config['members'][0]['path']=str(alias);config['members'][1]['path']=str(m)
        p=subprocess.run([str(binary),'raid','--config',str(save(case,config,'duplicate')),'--json'],capture_output=True,text=True);check(p.returncode and 'same file' in p.stderr,'duplicate member identity accepted');checks.append('duplicate member identities rejected')
        # Real MCP lifecycle plus recursively confined input and output paths.
        case=root/'mcp';config,_=encode(case,source.read_bytes(),'raid6',layout='pq_last');grants=[Path(m['path']) for m in config['members']];out=case/'output';client=MultiClient(binary,grants,case/'work',out)
        try:
            client.rpc('initialize',{'protocolVersion':'2025-11-25'});args={'config':config,'destination':str(out/'array.rescue-raid.json'),'request_id':'create'}
            job=client.tool('create_raid',args);check(client.tool('create_raid',args)==job,'RAID idempotency');done=client.wait(job['job_id']);check(done['state']=='completed','MCP create '+str(done));path=done['result']['source']
            done=client.wait(client.tool('start_scan',{'source':path,'request_id':'scan'})['job_id']);check(done['state']=='completed','MCP array scan '+str(done))
            done=client.wait(client.tool('recover_files',{'session_id':done['result']['session_id'],'ids':list(truth),'destination':str(out/'recovered'),'request_id':'restore'})['job_id']);check(done['state']=='completed','MCP array restore '+str(done))
            for item in done['result']['results']:check(digest(Path(item['path']))==truth[item['candidate_id']]['sha256'],'MCP RAID bytes')
            exact_files+=len(truth)
            search_args={'config':config,'options':{'stripe_sizes':[4096],'max_milliseconds':30000},'request_id':'search'}
            started=client.tool('search_raid',search_args);check(client.tool('search_raid',search_args)==started,'search idempotency')
            found=client.wait(started['job_id']);check(found['state']=='completed' and found['result']['candidates'],'MCP candidate alternatives')
            manifest=Path(path);saved=manifest.read_bytes();tampered=json.loads(saved);tampered['members'][0]['path']=str(source);manifest.write_text(json.dumps(tampered),encoding='utf-8')
            denied=client.wait(client.tool('start_scan',{'source':path,'request_id':'changed-manifest'})['job_id']);check(denied['state']=='failed' and 'allowlist' in denied['result']['error']['message'],'tampered manifest escaped source grants');manifest.write_bytes(saved)
            bad=copy.deepcopy(config);bad['members'][0]['path']=str(source)
            done=client.wait(client.tool('create_raid',{'config':bad,'destination':str(out/'forbidden.rescue-raid.json'),'request_id':'forbidden'})['job_id']);check(done['state']=='failed' and 'allowlist' in done['result']['error']['message'],'member escaped allowlist')
            done=client.wait(client.tool('create_raid',{'config':config,'destination':str(case/'outside.rescue-raid.json'),'request_id':'outside'})['job_id']);check(done['state']=='failed' and not (case/'outside.rescue-raid.json').exists(),'manifest output escaped')
        finally:client.close()
        checks.append('MCP create/scan/restore, idempotency, recursive member/output confinement')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':digest(binary),'platform':platform.platform(),'checks':len(checks),'cases':checks,'logical_arrays_byte_exact':exact_arrays,'files_byte_exact':exact_files,'source_unchanged':True,'scope':__doc__.strip()}
    if output:output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
