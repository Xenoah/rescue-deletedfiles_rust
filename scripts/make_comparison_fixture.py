"""Create three fixed commercial-comparison inputs using independent formatters.

Requires ntfs-3g (mkntfs, ntfscp, ntfsinfo), dosfstools, mtools, exfatprogs.
Only newly created 64 MiB regular files are modified. FAT32 uses real mdel;
NTFS and exFAT use explicitly described emulated deletion. Preserve the final
images and manifest for all products: timestamps/formatter IDs need not be
identical across different generations. No device or mount command is used.
"""
import argparse,os,subprocess,re,json,struct,math
from pathlib import Path
from e2e_formats import corpus_load,file_hash,exfat_set
from independent_ntfs import mark_deleted
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--corpus',type=Path,default=Path(__file__).resolve().parent.parent/'tests/corpus')
a=p.parse_args();root=a.output.resolve();root.mkdir(parents=True,exist_ok=False)
files=list(corpus_load(a.corpus.resolve()))
def run(*args):return subprocess.check_output(list(map(str,args)),stderr=subprocess.STDOUT,text=True)
manifest={'schema_version':1,'scope':__doc__.strip(),'cases':[]}
def record(source,truth,scope):
 manifest['cases'].append({'case':source.stem,'source':str(source),'source_sha256':file_hash(source),'scope':scope,'files':[{'id':key,'name':name,'relative_path':name,'size':len(e['data']),'sha256':e['sha256'],'original':str(e['path'])} for key,name,e in truth]})
 (root/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
source=root/'independent-ntfs.img'
with source.open('xb') as f:f.truncate(64*1024*1024)
run('mkntfs','-F','-Q','-s',512,'-c',4096,source)
refs=[]
for e in files:
 run('ntfscp',source,e['path'],'/'+e['path'].name)
 refs.append(int(re.search(r'Dumping Inode (\d+)',run('ntfsinfo','-F','/'+e['path'].name,source)).group(1)))
truth=[(f'mft-{ref}-{mark_deleted(source,ref)}',e['path'].name,e) for ref,e in zip(refs,files)]
record(source,truth,'ntfs-3g generated; emulated deletion clears MFT in-use bit; allocation bitmap and directory index retained')
source=root/'independent-fat32.img'
with source.open('xb') as f:f.truncate(64*1024*1024)
run('mkfs.fat','-F',32,'-s',1,source);truth=[]
for i,e in enumerate(files):
 name=f'fixture-{i:03d}.'+e['extension'];run('mcopy','-i',source,e['path'],'::'+name);truth.append((str(i),name,e))
for _,name,_ in truth:run('mdel','-i',source,'::'+name)
record(source,truth,'dosfstools + mtools; actual mdel deletion clears multi-cluster chains; same denominator includes unrecoverable metadata cases')
source=root/'independent-exfat.img'
with source.open('xb') as f:f.truncate(64*1024*1024)
run('mkfs.exfat','-L','RESCUE',source);truth=[]
with source.open('r+b') as f:
 boot=f.read(512);sector=1<<boot[108];cluster=sector<<boot[109];heap=struct.unpack_from('<I',boot,88)[0]*sector
 root_cluster=struct.unpack_from('<I',boot,96)[0];address=lambda c:heap+(c-2)*cluster
 f.seek(address(root_cluster));directory=f.read(cluster);free=next(at for at in range(0,len(directory),32) if directory[at]==0)
 bm=next(directory[at:at+32] for at in range(0,len(directory),32) if directory[at]==0x81)
 f.seek(address(struct.unpack_from('<I',bm,20)[0]));bitmap=f.read(struct.unpack_from('<Q',bm,24)[0]);cursor=max(root_cluster+128,2048)
 for e in files:
  count=math.ceil(e['bytes']/cluster);assert all(not bitmap[(c-2)//8]&(1<<((c-2)%8)) for c in range(cursor,cursor+count))
  slots=b''.join(exfat_set(e['path'].name,cursor,e['bytes']));assert free+len(slots)+32<=cluster
  offset=address(root_cluster)+free;f.seek(offset);f.write(slots);free+=len(slots)
  f.seek(address(cursor));f.write(e['data']);cursor+=count+2;truth.append((f'exfat-{offset}',e['path'].name,e))
 f.seek(address(root_cluster)+free);f.write(bytes(32))
record(source,truth,'exfatprogs formatted; emulated deleted contiguous directory entries inserted in free clusters')
print('created independent NTFS/FAT32/exFAT')
