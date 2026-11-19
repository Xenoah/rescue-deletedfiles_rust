"""Compare libewf, AFFLIB and VHDX images against an ext4 disk with real file deletions."""
import argparse
import hashlib
from pathlib import Path
import tempfile
from e2e import check, run
from e2e_extended_filesystems import command, make_deleted, CORPUS


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify(binary,root):
    files={f'sample.{ext}':(CORPUS/f'sample.{ext}').read_bytes() for ext in ('gif','bmp','webp','wav','sqlite','txt')}
    files['opaque'] = bytes(range(251))*31
    for name,data in files.items():
        (root/name).write_bytes(data)
    raw=make_deleted(root,'ext4',files).rename(root/'ext4.raw')
    expected=sha(raw)
    evidence={}
    def recover(label,source,kind):
        result=run(binary,'filesystem','--source',source,'--image-type',kind,'--fs-type','ext4','--all',
                   '--destination',root/(label+'-files'),'--json')
        rows=result['results']
        check(len(rows)==len(files) and {r['original_path'] for r in rows}==files.keys(),label)
        for r in rows:
            b=files[r['original_path']]
            check(Path(r['path']).read_bytes()==b and r['sha256']==hashlib.sha256(b).hexdigest(),r)
    def verify_image(label,segments,kind=None,ewf=False):
        before={p:sha(p) for p in segments}
        output=root/(label+'-decoded.raw')
        flags=['--image-type',kind] if kind else []
        extra=[] if len(segments)==1 else [v for path in segments[1:] for v in ('--segment',path)]
        result=run(binary,'import-image','--source',segments[0],*extra,*flags,'--output',output,'--json')
        check(result['sha256']==expected and sha(output)==expected and output.read_bytes()==raw.read_bytes(),label)
        if ewf:
            check(result['reader']=='libewf' and result['logical_sha256_verified'],result)
        else:
            recover(label+'-direct',segments[0],kind)
        recover(label+'-raw',output,'raw')
        check({p:sha(p) for p in segments}==before and sha(raw)==expected,'source changed')
        evidence[label]={'segments':len(segments),'logical_sha256':expected,
                         'deleted_files':{name:hashlib.sha256(data).hexdigest() for name,data in files.items()}}
        print(label,len(segments),'segments; RAW and seven deleted files byte-exact',flush=True)
    for subformat in ('dynamic','fixed'):
        image=root/(subformat+'.vhdx')
        command('qemu-img','convert','-f','raw','-O','vhdx','-o','subformat='+subformat,raw,image)
        verify_image('vhdx-'+subformat,[image],'vhdx')
    image=root/'forensic.aff'
    command('affconvert','-o',image,raw)
    verify_image('aff',[image],'aff')
    for label,compression,segment_mb,kind in [('single','fast',64,None),('multiple','none',1,'ewf-verified')]:
        with raw.open('rb') as source:
            command('ewfacquirestream','-q','-c',compression,'-S',str(segment_mb*1024*1024),
                    '-t',root/label,stdin=source)
        segments=sorted(root.glob(label+'.E*'))
        check(len(segments)==1 if label=='single' else len(segments)>1,segments)
        verify_image(label,segments,kind,True)
    broken=root/'incomplete';broken.mkdir()
    source=broken/segments[0].name;source.write_bytes(segments[0].read_bytes())
    failed=broken/'incomplete.raw'
    run(binary,'import-image','--source',source,'--image-type','ewf-verified','--output',failed,'--json',expected=4)
    check(not failed.exists() and sha(raw)==expected,'incomplete EWF output')
    return evidence


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(p.parse_args().binary.resolve(),Path(tmp))
