"""Actual mkfs.fat + mcopy + mdel deletion on FAT12 and FAT16 images."""
import argparse,hashlib,json,os,struct,subprocess,tempfile
from pathlib import Path
from e2e import check,run
from make_fat_fixture import file_hash


def main(binary,output=None):
    exact=0;cases=[]
    env={**os.environ,'MTOOLS_SKIP_CHECK':'1'}
    with tempfile.TemporaryDirectory(prefix='rescue-independent-classic-') as temporary:
        root=Path(temporary)
        for bits,size in [(12,1440*1024),(16,16*1024*1024)]:
            image=root/f'fat{bits}.img'
            with image.open('xb') as f:f.truncate(size)
            subprocess.run(['mkfs.fat','-F',str(bits),str(image)],check=True,env=env,capture_output=True)
            boot=image.read_bytes()[:512];cluster=struct.unpack_from('<H',boot,11)[0]*boot[13]
            payloads={'SMALL.TXT':b'Independent deletion\n','復元対象の長い名前.txt':'日本語の小さいファイル\n'.encode(),
                      'LARGE.BIN':bytes(range(251))*(cluster*3//251+1)+b'END',
                      '大きいファイル.txt':b'large original content\n'*(cluster*3//23+1)}
            for i,(name,data) in enumerate(payloads.items()):
                file=root/f'payload-{bits}-{i}';file.write_bytes(data)
                subprocess.run(['mcopy','-i',str(image),str(file),'::'+name],check=True,env=env,capture_output=True)
            for name in payloads:subprocess.run(['mdel','-i',str(image),'::'+name],check=True,env=env,capture_output=True)
            before=file_hash(image)
            scan=run(binary,'scan','--indexed','--source',image,'--work-dir',root/f'w{bits}','--estimate-fat-contiguous','--json')
            result=run(binary,'recover','--session',scan['session'],'--all','--destination',root/f'o{bits}','--json')
            wanted=sorted(hashlib.sha256(data).hexdigest() for data in payloads.values())
            got=sorted(file_hash(Path(row['path'])) for row in result['results'] if row['status']=='completed')
            check(wanted==got and len(result['results'])==4,'independent FAT original bytes differ')
            check(sum(row['extent_evidence']=='estimated_contiguous' for row in result['results'])==2,'cleared multi-cluster files not labelled estimated')
            check(file_hash(image)==before,'independent source modified');exact+=4
            cases.append({'filesystem':f'fat{bits}','bytes':size,'cluster_bytes':cluster,'files_byte_exact':4,'estimated_files_byte_exact':2,'source_sha256':before})
    report={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'files_byte_exact':exact,'source_unchanged':True,'cases':cases,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(report,ensure_ascii=False));return report


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
