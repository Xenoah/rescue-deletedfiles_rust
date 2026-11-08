"""Retrieve provenance from the tag's native CI artifact, not release assets."""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    p=argparse.ArgumentParser()
    p.add_argument('--tag',required=True)
    p.add_argument('--run',required=True)
    p.add_argument('--platform',required=True,choices=['windows','linux','macos'])
    p.add_argument('--arch',required=True,choices=['x64','arm64'])
    p.add_argument('--output',required=True,type=Path)
    a=p.parse_args()
    repo=os.environ['GH_REPO']
    commit=subprocess.check_output(['git','rev-parse',a.tag+'^{commit}'],text=True).strip()
    if not a.run:
        runs=json.loads(subprocess.check_output(['gh','api',f'repos/{repo}/actions/workflows/ci.yml/runs?head_sha={commit}&event=push&per_page=50'],text=True))['workflow_runs']
        matches=[r for r in runs if r['head_branch']==a.tag and r['head_sha']==commit and r['conclusion']=='success']
        if not matches:raise ValueError('No successful native release run for this immutable tag')
        a.run=str(matches[0]['id'])
    if not a.run.isdigit():raise ValueError('A numeric native release workflow run is required')
    run=json.loads(subprocess.check_output(['gh','api',f'repos/{repo}/actions/runs/{a.run}'],text=True))
    if run['head_sha']!=commit or run['path']!='.github/workflows/ci.yml' or run['event']!='push':
        raise ValueError('Native workflow provenance does not match the immutable release tag')
    # Dispatch occurs at the end of the release job, before its run is marked
    # completed. Only the successful native matrix artifacts are consumed.
    subprocess.run(['gh','run','download',a.run,'--repo',repo,'--name',
                    f'native-{a.platform}-{a.arch}','--dir',str(a.output)],check=True)
    print(json.dumps({'tag':a.tag,'commit':commit,'native_run':a.run,
                      'artifact':f'native-{a.platform}-{a.arch}'}))


if __name__=='__main__':
    main()
