"""Capture unencrypted archives with official independent RAR writers and verify every member."""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--rar6',required=True,type=Path)
p.add_argument('--rar7',required=True,type=Path)
p.add_argument('--unrar',required=True,type=Path)
p.add_argument('--output',required=True,type=Path)
args=p.parse_args()
corpus=Path(__file__).resolve().parents[1]/'corpus/files'
cases=[
 ('rar4-store',4,'-m0',['sample.pdf','sample.png']),
 ('rar4-normal',4,'-m3',['sample.rtf','sample.wav','sample.json']),
 ('rar4-solid',4,'-m5 -s',['sample.xml','sample.html','sample.bmp','sample.json']),
 ('rar4-unicode',4,'-m3',['sample.docx','sample.sqlite']),
 ('rar4-many',4,'-m1',['sample.avi','sample.pptx','sample.tar','sample.flac']),
 ('rar5-store',5,'-m0',['sample.pdf','sample.png']),
 ('rar5-fast',5,'-m1',['sample.json','sample.rtf']),
 ('rar5-normal',5,'-m3',['sample.wav','sample.bmp','sample.xml']),
 ('rar5-solid',5,'-m5 -s',['sample.html','sample.flac','sample.tar','sample.docx']),
 ('rar5-blake',5,'-m3 -htb',['sample.sqlite','sample.avi','sample.pptx']),
]
env=dict(os.environ,LC_ALL='C.UTF-8')
dest=args.output.resolve();dest.mkdir(parents=True,exist_ok=False);facts={}
for case,family,flags,files in cases:
 tool=(args.rar6 if family==4 else args.rar7).resolve()
 with tempfile.TemporaryDirectory() as td:
  root=Path(td); originals={}
  for idx,filename in enumerate(files):
   data=(corpus/filename).read_bytes()
   name=('試験/' if case.endswith('unicode') else 'payload/')+filename
   p=root/name;p.parent.mkdir(exist_ok=True);p.write_bytes(data)
   os.utime(p,(1600000000,1600000000));originals[name]=hashlib.sha256(data).hexdigest()
  out=dest/(case+'.rar');assert not out.exists()
  subprocess.run([tool,'a','-idq','-ma'+str(family),*flags.split(),out,*originals],cwd=root,env=env,check=True)
  subprocess.run([args.unrar.resolve(),'t','-inul',out],cwd=root,env=env,check=True)
  outdir=root/'unpacked';outdir.mkdir(); subprocess.run([args.unrar.resolve(),'x','-inul',out,str(outdir)+'/'],cwd=root,env=env,check=True)
  archived={}
  for file in outdir.rglob('*'):
   if file.is_file():
    src=root/next(n for n in originals if Path(n).name==file.name)
    assert file.read_bytes()==src.read_bytes()
    archived[str(file.relative_to(outdir))]=hashlib.sha256(file.read_bytes()).hexdigest()
  assert len(archived)==len(files)
  facts[out.name]=dict(bytes=out.stat().st_size,sha256=hashlib.sha256(out.read_bytes()).hexdigest(),members=archived,format=family,flags=flags)
  print(case,len(archived),out.stat().st_size,flush=True)
(dest/'manifest.json').write_text(json.dumps(facts,sort_keys=True,indent=2,ensure_ascii=False)+'\n')
