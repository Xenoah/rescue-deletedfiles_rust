"""Run Rescue on the same immutable images passed to commercial products.

Three warm-cache repetitions; no speed comparison to other products is implied.
The first repetition's actual output files are used by score_comparison.py.
All candidates, including unsupported ones, remain in the saved evidence.
"""
import argparse
import json
from pathlib import Path
import subprocess
import time
from score_comparison import digest, read_manifest


def main(a):
    a.work.mkdir(parents=True,exist_ok=False)
    root=a.outputs/a.product;root.mkdir(parents=True,exist_ok=False)
    report={'schema_version':1,'version':subprocess.check_output([str(a.binary),'--version'],text=True).strip(),
            'binary_sha256':digest(a.binary),'scope':__doc__.strip(),'cases':[]}
    for case in read_manifest(a.manifest)['cases']:
        if a.case and case['case'] not in a.case:continue
        source=Path(case['source'])
        if digest(source)!=case['source_sha256']:raise ValueError('input changed before scan')
        runs=[]
        for repetition in range(3):
            base=a.work/case['case']/str(repetition);base.mkdir(parents=True)
            def run(name,*args):
                start=time.perf_counter()
                p=subprocess.run([str(a.binary),*map(str,args),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=120)
                elapsed=time.perf_counter()-start
                result={'exit':p.returncode,'seconds':elapsed,'stdout':json.loads(p.stdout) if p.stdout else None,'stderr':p.stderr}
                (base/(name+'.json')).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
                if p.returncode not in (0,4,5):raise RuntimeError(p.stderr)
                return result
            scan=run('scan','scan','--source',source,'--work-dir',base/'session')
            session=scan['stdout']['session']
            listed=run('candidates','results','--session',session,'--limit',1000)
            destination=root/case['case'] if repetition==0 else base/'recovered'
            recovered=run('recover','recover','--session',session,'--all','--destination',destination)
            runs.append({'repetition':repetition,'scan_seconds':scan['seconds'],'list_seconds':listed['seconds'],
                         'recover_seconds':recovered['seconds'],'candidates':listed['stdout']['candidates'],
                         'recovery':recovered['stdout']})
        unchanged=digest(source)==case['source_sha256']
        if not unchanged:raise ValueError('source changed during comparison')
        report['cases'].append({'case':case['case'],'source_sha256':case['source_sha256'],'source_unchanged':unchanged,'runs':runs})
        print(case['case'],[r['scan_seconds'] for r in runs],flush=True)
    (a.work/'results.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--manifest',type=Path,required=True)
    p.add_argument('--outputs',type=Path,required=True);p.add_argument('--product',default='rescue');p.add_argument('--case',action='append')
    p.add_argument('--work',type=Path,required=True);main(p.parse_args())
