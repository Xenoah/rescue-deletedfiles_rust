"""Download checksum-verified published Windows CLI and test its exact bytes."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import urllib.request
import zipfile
from e2e_formats import main


def download(url,limit):
    with urllib.request.urlopen(url,timeout=60) as response:data=response.read(limit+1)
    if len(data)>limit:raise RuntimeError('download exceeds expected size limit')
    return data


def test(tag,output):
    if not re.fullmatch(r'v\d+\.\d+\.\d+-alpha\.\d+',tag):raise ValueError('expected a release alpha tag')
    version=tag.removeprefix('v');name=f'rescue-{version}-windows-x64.zip'
    base=f'https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/{tag}/'
    manifest=download(base+'SHA256SUMS.txt',65536).decode('ascii')
    expected=next(line.split()[0] for line in manifest.splitlines() if line.split()[1]==name)
    archive=download(base+name,64*1024*1024);digest=hashlib.sha256(archive).hexdigest()
    if digest!=expected:raise RuntimeError('published ZIP checksum mismatch')
    output.mkdir(parents=True,exist_ok=True);target=output/'published';target.mkdir(exist_ok=False)
    with zipfile.ZipFile(io.BytesIO(archive)) as z:
        if z.testzip() is not None:raise RuntimeError('published ZIP CRC mismatch')
        (target/'rescue.exe').write_bytes(z.read('rescue.exe'))
    binary=(target/'rescue.exe').resolve()
    observed=subprocess.check_output([str(binary),'--version'],text=True,encoding='utf-8').strip()
    if observed!='rescue '+version:raise RuntimeError('published binary version mismatch: '+observed)
    result=main(binary,Path(__file__).resolve().parent.parent/'tests/corpus',output,False)
    result['published_release']={'tag':tag,'zip_sha256':digest,'executable_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
                                 'download_url':base+name,'version_output':observed}
    (output/'results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'published_release_tested':tag,'zip_sha256':digest,'binary_sha256':result['binary_sha256']}))


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--tag',required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();test(args.tag,args.output.resolve())
