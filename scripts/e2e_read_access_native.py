"""Windows broker handoff on an independently encoded, owned read-only VHD.

Checks the actual ShellExecute helper, kernel-granted rights, child-preview handle
transfer and byte-exact recovery. Records the parent token; an elevated runner
does not establish interactive UAC consent or an unelevated desktop UI result.
"""
import argparse, hashlib, io, json, platform, subprocess, tempfile, time
from pathlib import Path
from PIL import Image
from e2e import check, run
from e2e_disks import native_source, disk_image
from e2e_formats import corpus_load, ntfs_image, file_hash


def main(binary, output, limited=False):
    check(platform.system()=='Windows','Windows broker native test requires Windows')
    output.parent.mkdir(parents=True,exist_ok=True)
    grants=[];cases=[]
    def invoke(arguments):
        command=[str(binary),*map(str,arguments)]
        if limited:
            from windows_restricted import run_limited
            return run_limited(command,timeout=60)
        return subprocess.run(command,capture_output=True,text=True,encoding='utf-8',timeout=60)
    def permitted(*args):
        p=invoke(['--request-read-access',*args])
        check(p.returncode==0,'broker command failed: '+p.stdout+' '+p.stderr)
        accesses=[json.loads(line)['read_access'] for line in p.stderr.splitlines() if line.startswith('{"read_access":')]
        check(len(accesses)==1,'missing or ambiguous read permission receipt: '+p.stderr)
        access=accesses[0]
        check(access['read_only'] and access['helper_exited'] and access['granted_access'] & 1 and not access['granted_access'] & 0xd0156,access)
        if limited:check(not access['parent_elevated'],'the parser parent was elevated')
        grants.append(access)
        return json.loads(p.stdout)
    with tempfile.TemporaryDirectory(prefix='rescue-owned-broker-',dir=output.parent) as temporary:
        root=Path(temporary).resolve();volume=root/'ntfs.raw';disk=root/'disk.raw'
        corpus=list(corpus_load(Path(__file__).resolve().parent.parent/'tests/corpus'))
        original,_=ntfs_image(volume,corpus,'fragmented')
        disk_image(disk,[volume],512,'mbr')
        # Use a conventional VHD capacity while preserving the independent MBR
        # partition bytes. The extra logical source bytes are verified zeros.
        with disk.open('r+b') as f:f.truncate(max(disk.stat().st_size,64*1024*1024))
        before=file_hash(disk)
        truth={'p1:'+key:value for key,value in original.items()}
        with native_source(disk,root) as (_,source,backing):
            backing_before=file_hash(backing)
            # Mount completion and DevicePath publication can precede storage
            # geometry readiness. Probe only this owned VHD with the existing
            # admin runner; later limited-parent checks still require denial.
            for attempt in range(40):
                baseline=subprocess.run([str(binary),'inspect','--source',str(source),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=15)
                if baseline.returncode==0:break
                if 'IO_ERROR' not in baseline.stderr:break
                time.sleep(.25)
            check(baseline.returncode==0,'owned VHD baseline read failed before broker: '+baseline.stdout+' '+baseline.stderr)
            layout=json.loads(baseline.stdout)['layout']
            check(layout['bytes']==disk.stat().st_size and len(layout['regions'])==1,'owned VHD baseline geometry differs')
            cases.append('owned_vhd_readable_before_permission_handoff')
            if limited:
                denied=invoke(['inspect','--source',source,'--json'])
                check(denied.returncode==4 and 'IO_ERROR' in denied.stderr and 'os error 5' in denied.stderr,'limited parent unexpectedly read the device without a grant: '+denied.stdout+denied.stderr)
                cases.append('unelevated_parent_cannot_open_disk_without_explicit_permission')
            inspected=permitted('inspect','--source',source,'--json')
            check(len(inspected['layout']['regions'])==1,'disk query through transferred handle')
            scanned=permitted('scan','--indexed','--source',source,'--work-dir',root/'work','--json')
            rows=run(binary,'results','--session',scanned['session'],'--limit',1000,'--json')['candidates']
            check({c['id'] for c in rows}==set(truth),'broker scan differs from oracle')
            recovered=permitted('recover','--session',scanned['session'],'--all','--destination',root/'out','--json')['results']
            check(len(recovered)==len(truth) and all(r['status']=='completed' and Path(r['path']).read_bytes()==truth[r['candidate_id']]['data'] for r in recovered),'broker recovery differs')
            cases.append('actual_read_only_helper_exits_before_scan_and_recovery')
            previews=0
            for key,item in truth.items():
                if item['path'].name not in ['sample.png','sample.jpg','sample.txt']:continue
                shown=permitted('preview','--session',scanned['session'],'--id',key,'--destination',root/'preview','--json')
                if item['path'].suffix=='.txt':
                    check(Path(shown['path']).read_bytes()==item['data'],'broker preview text differs')
                else:
                    with Image.open(shown['path']) as actual,Image.open(io.BytesIO(item['data'])) as expected:
                        check(actual.size==expected.size,'broker preview image size')
                        differences=[abs(a-b) for a,b in zip(actual.convert('RGBA').tobytes(),expected.convert('RGBA').tobytes())]
                        check(max(differences,default=0)<=(3 if item['path'].suffix=='.jpg' else 0),'broker preview pixels')
                previews+=1
            check(previews==3,'missing broker preview formats');cases.append('preview_worker_receives_only_read_handles')
            acquired=permitted('image','--source',source,'--destination',root/'acquired','--json')
            check(file_hash(Path(acquired['image']))==before,'broker acquisition bytes differ');cases.append('complete_native_disk_acquisition_byte_exact')
            for invalid in [str(volume),r'\\.\pipe\rescue-owned-not-a-disk',str(source)+'\\child']:
                failed=subprocess.run([str(binary),'--request-read-access','inspect','--source',invalid,'--json'],capture_output=True,text=True,encoding='utf-8',timeout=10)
                check(failed.returncode==2 and 'INVALID_REQUEST' in failed.stderr and 'read_access' not in failed.stderr,'non-device source requested elevation')
            cases.append('files_named_pipes_and_device_subpaths_rejected_before_elevation')
            check(file_hash(backing)==backing_before,'owned VHD changed during broker use')
        check(file_hash(backing)==backing_before and file_hash(disk)==before,'owned source changed on detach')
        cases.append('source_and_backing_unchanged')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
            'binary_sha256':file_hash(binary),'checks':len(cases),'cases':cases,'files_byte_exact':len(truth),
            'previews_verified':previews,'source_unchanged':True,'grants':grants,
            'unelevated_parent_tested':all(not g['parent_elevated'] for g in grants),
            'interactive_uac_tested':False,'scope':__doc__.strip()}
    output.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--limited',action='store_true');a=p.parse_args()
    if a.limited:
        from windows_restricted import silent_admin_consent
        with silent_admin_consent() as policy:
            result=main(a.binary.resolve(),a.output.resolve(),limited=True)
        result['uac_policy']=policy
        a.output.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    else:main(a.binary.resolve(),a.output.resolve())
