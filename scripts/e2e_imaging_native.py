"""Capacity and permission failures on a disposable OS-native mounted volume.

Only the uniquely created fixture from e2e_restore_native is formatted. Filling
and deleting files is confined to that fixture; no host volume is filled.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
from e2e import check, run
from e2e_imaging import sha
from e2e_restore_native import mounted_fixture, sync_volume


def main(binary,output):
    output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='rescue-owned-image-failure-',dir=output.parent) as tmp:
        root=Path(tmp).resolve();source=root/'source.raw'
        with source.open('xb') as f:
            for n in range(32):f.write(bytes([n])*1024*1024)
        before=sha(source)
        with mounted_fixture(root) as (_,mount,filesystem):
            free=shutil.disk_usage(mount).free;fill=free-12*1024*1024
            check(0<fill<256*1024*1024,'unexpected owned fixture capacity')
            filler=mount/'owned-space-reservation.bin'
            with filler.open('xb') as f:
                while fill:
                    n=min(fill,1024*1024);f.write(b'F'*n);fill-=n
                f.flush();os.fsync(f.fileno())
            sync_volume(mount)
            destination=mount/'acquisition'
            command=[str(binary),'image','--source',str(source),'--destination',str(destination),'--json']
            p=subprocess.run(command,capture_output=True,text=True,encoding='utf-8',timeout=90)
            check(p.returncode==4,'capacity limit unexpectedly succeeded: '+p.stdout+p.stderr)
            error=json.loads(p.stderr)['error'];check(error['code']=='IO_ERROR' and any('os error '+str(n) in error['message'] for n in [28,112,39]),'not a native capacity error: '+str(error))
            image=destination/'image.rescue.raw';coverage=run(binary,'read-map','--image',image,'--limit',1000,'--json')
            check(0<coverage['copied_bytes']<source.stat().st_size and coverage['unattempted_bytes']>0,'failed write committed as copied')
            with image.open('rb') as f:
                for r in coverage['ranges']:
                    if r['state']!='copied':continue
                    f.seek(r['offset']);data=f.read(r['length']);check(hashlib.sha256(data).hexdigest()==r['sha256'],'durable range corrupted')
            # Release only the owned filler file, retaining all acquisition bytes and map records.
            filler.unlink();sync_volume(mount)
            resumed=run(binary,*command[1:],'--resume');check(resumed['state']=='completed' and sha(image)==before,'resume after capacity failure differs')
            # A separate owned directory verifies actual OS access denial.
            # exFAT has no per-directory POSIX permissions; use an owned directory
            # on the runner filesystem for Unix permission semantics.
            denied=(mount if platform.system()=='Windows' else root)/'denied';denied.mkdir()
            if platform.system()=='Windows':
                user=subprocess.check_output(['whoami'],text=True).strip()
                subprocess.run(['icacls',str(denied),'/deny',user+':(W)'],check=True,capture_output=True)
            else:denied.chmod(0)
            try:
                p=subprocess.run([str(binary),'image','--source',str(source),'--destination',str(denied),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=30)
                check(p.returncode==4 and json.loads(p.stderr)['error']['code']=='IO_ERROR','OS permission denial not propagated')
            finally:
                if platform.system()=='Windows':subprocess.run(['icacls',str(denied),'/remove:d',user],check=True,capture_output=True)
                else:denied.chmod(0o700)
            check(not list(denied.iterdir()),'permission failure created output')
            check(sha(source)==before,'original source changed')
            result={'status':'passed','schema_version':1,'os':platform.system(),'architecture':platform.machine(),'version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':sha(binary),'filesystem':filesystem,'native_capacity_error':error,'copied_bytes_before_capacity_failure':coverage['copied_bytes'],'resume_after_capacity_failure_byte_exact':True,'native_permission_denied':True,'source_unchanged':True,'scope':'Only a newly created owned mounted virtual volume was filled. Failed-write durability, freeing owned filler, resume and OS permission denial.'}
        output.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8');print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();main(a.binary.resolve(),a.output.resolve())
