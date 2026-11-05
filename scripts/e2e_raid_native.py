"""Linux mdadm/kernel oracle on exclusively owned disposable loop images.

Creates five real kernel arrays, writes known logical bytes, stops/detaches them,
then reconstructs their regular member images with Rescue. Never assembles,
formats, stops or writes an existing host array/device. No hardware fault claim.
"""
import argparse,copy,hashlib,json,os,platform,subprocess,tempfile,time,uuid
from pathlib import Path
from e2e import check,run
from make_fixture import build

def call(*args):
    p=subprocess.run([str(a) for a in args],capture_output=True,text=True,encoding='utf-8',timeout=90)
    check(p.returncode==0,f'{args[0]} failed: {p.stdout[-2000:]} {p.stderr[-2000:]}')
    return p.stdout.strip()

def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()

def owned_loop(path):
    device=call('sudo','losetup','--find','--show',path)
    check(device.startswith('/dev/loop') and device[9:].isdigit(),'unexpected loop path')
    backing=call('sudo','losetup','--noheadings','--output','BACK-FILE',device)
    check(Path(backing).resolve()==path.resolve(),'loop backing identity mismatch')
    return device

def main(binary,output=None):
    check(platform.system()=='Linux','kernel md oracle requires Linux')
    cases=[];exact_arrays=0;exact_files=0
    with tempfile.TemporaryDirectory(prefix='rescue-owned-md-') as tmp:
        root=Path(tmp);source=root/'ntfs.raw';truth=build(source);payload=source.read_bytes()
        profiles=[('raid5',x) for x in ['left_symmetric','right_symmetric','left_asymmetric','right_asymmetric']]+[('raid6','pq_last')]
        for number,(level,layout) in enumerate(profiles):
            case=root/f'case-{number}';case.mkdir();loops=[];created=False
            md=Path('/dev/md')/('rescue_test_'+uuid.uuid4().hex[:12])
            check(not os.path.lexists(md),'test array path already exists')
            paths=[]
            try:
                for index in range(4):
                    path=case/f'member-{index}.raw'
                    with path.open('xb') as f:f.truncate(16*1024*1024)
                    paths.append(path);loops.append(owned_loop(path))
                kernel_layout='parity-last' if level=='raid6' else layout.replace('_','-')
                created=True
                call('sudo','mdadm','--create',md,'--run','--force','--metadata=1.2','--level='+level[4:],
                     '--raid-devices=4','--layout='+kernel_layout,'--chunk=64','--size=8192','--data-offset=1M','--bitmap=none',*loops)
                canonical=md.resolve(strict=True);sysfs=Path('/sys/class/block')/canonical.name/'md'
                check(sysfs.is_dir(),'missing owned array sysfs')
                deadline=time.monotonic()+90
                while (sysfs/'sync_action').read_text().strip()!='idle':
                    check(time.monotonic()<deadline,'owned md initial sync exceeded 90 seconds')
                    time.sleep(.1)
                data=2 if level=='raid6' else 3
                length=int(call('sudo','blockdev','--getsize64',md));check(length==data*8*1024*1024,'kernel capacity differs')
                component=int((sysfs/'component_size').read_text())*512
                check(component==8*1024*1024,'kernel component length differs')
                members=[]
                for index,(device,path) in enumerate(zip(loops,paths)):
                    item=sysfs/('dev-'+Path(device).name)
                    check(int((item/'slot').read_text())==index,'kernel reordered members')
                    offset=int((item/'offset').read_text())*512
                    check(offset==1024*1024,'kernel data offset differs')
                    members.append({'path':str(path),'offset':offset,'length':component})
                expected=(payload+bytes(range(256))*((length-len(payload)+255)//256))[:length]
                logical=case/'logical.raw';logical.write_bytes(expected)
                call('sudo','dd','if='+str(logical),'of='+str(md),'bs=1M','conv=fsync','status=none')
                call('sudo','blockdev','--flushbufs',md)
                call('sudo','mdadm','--stop',md);created=False
            finally:
                if created and md.exists():
                    owned_sysfs=Path('/sys/class/block')/md.resolve().name/'slaves'
                    check(owned_sysfs.is_dir() and {p.name for p in owned_sysfs.iterdir()}=={Path(p).name for p in loops},'refusing cleanup of an unverified array')
                    call('sudo','mdadm','--stop',md)
                for device in reversed(loops):call('sudo','losetup','--detach',device)
            before=[digest(p) for p in paths]
            config={'schema_version':1,'level':level,'layout':layout,'sector_size':512,'stripe_bytes':65536,'members':members}
            trials=[(),(0,)] if level=='raid5' else [(),(0,1),(0,3),(2,3)]
            for trial,missing in enumerate(trials):
                selected=copy.deepcopy(config)
                for index in missing:selected['members'][index]['path']=None
                manifest=case/f'input-{trial}.rescue-raid.json';manifest.write_text(json.dumps(selected),encoding='utf-8')
                image=run(binary,'image','--source',manifest,'--destination',case/f'acquired-{trial}','--block-size',1048576,'--retries',0,'--json',timeout=120)
                check(image['state']=='completed' and Path(image['image']).read_bytes()==expected,'kernel logical array differs')
                exact_arrays+=1
                scan=run(binary,'scan','--source',manifest,'--work-dir',case/f'work-{trial}','--json',timeout=90)
                recovered=run(binary,'recover','--session',scan['session'],'--all','--destination',case/f'out-{trial}','--json',timeout=90)['results']
                check({r['candidate_id'] for r in recovered}==set(truth),'kernel array file enumeration differs')
                check(all(r['status']=='completed' and digest(Path(r['path']))==truth[r['candidate_id']]['sha256'] for r in recovered),'kernel array file bytes differ')
                exact_files+=len(recovered)
            check([digest(p) for p in paths]==before,'Rescue changed member bytes')
            cases.append({'level':level,'layout':layout,'kernel_layout':kernel_layout,'member_offset':1024*1024,'member_length':component,'logical_length':length,'missing_slots':[list(x) for x in trials],'member_sha256':before})
            print(f'Kernel md: {level}/{layout} passed',flush=True)
    proof={'status':'passed','version':call(binary,'--version'),'binary_sha256':digest(binary),'kernel':platform.release(),
           'oracle':'Linux mdadm + kernel md, real newly-created loop devices','arrays_created':len(profiles),
           'logical_arrays_byte_exact':exact_arrays,'files_byte_exact':exact_files,'source_unchanged':True,'cases':cases,'scope':__doc__.strip()}
    if output:output.parent.mkdir(parents=True,exist_ok=True);output.write_text(json.dumps(proof,indent=2)+'\n')
    print(json.dumps(proof));return proof
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
