"""Restore files actually unlinked after an APFS snapshot and distinguish live paths."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import tempfile
from e2e import check, run

GOLD = Path(__file__).resolve().parents[1]/'tests/apfs-reference'


def file_hash(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()


def verify(binary,root):
    meta=json.loads((GOLD/'manifest.json').read_text())
    packed=GOLD/'snapshot.raw.xz'
    check(file_hash(packed)==meta['compressed_sha256'],'reference data')
    disk=root/'volume.raw'
    with lzma.open(packed,'rb') as src,disk.open('wb') as dst:shutil.copyfileobj(src,dst)
    check(file_hash(disk)==meta['sha256'] and disk.stat().st_size==meta['raw_bytes'],'reference disk')
    front=root/'partition.001';prefix=2048*512
    with front.open('wb') as f:f.truncate(prefix)
    def exercise(label,src,flags):
        image=['--source',src,*flags]
        inventory=run(binary,'apfs',*image,'--json')['volumes']
        v,=inventory
        check(meta['volume'] in v['name_and_role'] and v['volume_block']==meta['volume_block'],inventory)
        snap,=v['snapshots']
        check(snap['xid']==meta['snapshot_xid'] and meta['snapshot'] in snap['description'] and not snap['dataless'],snap)
        flags=['--snapshot-xid',str(snap['xid'])]
        if label!='bare':flags+=['--volume-block',str(v['volume_block'])]
        ls=run(binary,'apfs',*image,*flags,'--json')
        found={p:r['id'] for r in ls['candidates'] for p in r['original_paths']}
        check(found.keys()==meta['original_files'].keys() and ls['evidence']=='snapshot_only_paths',ls)
        dest=root/(label+'-out')
        results=run(binary,'apfs',*image,*flags,'--all','--destination',dest,'--json')['results']
        checks={r['original_paths'][0]:file_hash(Path(r['path'])) for r in results}
        check(checks==meta['original_files'] and all(r['sha256']==checks[r['original_paths'][0]] for r in results),results)
        pick=next(iter(found))
        row,=run(binary,'apfs',*image,*flags,'--id',found[pick],'--destination',root/(label+'-one'),'--json')['results']
        check(row['sha256']==meta['original_files'][pick],row)
        collision=run(binary,'apfs',*image,*flags,'--all','--destination',dest,'--json',expected=5)
        check(len(collision['results'])==len(found) and all('error' in r for r in collision['results']),collision)
        print(label,len(checks),'historical file hashes matched; live and later files excluded',flush=True)
        return checks
    proof={'bare':exercise('bare',disk,[])}
    # The source paths concatenate into a whole disk with the synthetic partition offset.
    proof['offset-split']=exercise('offset-split',front,['--segment',disk,'--image-type','raw','--sector-offset','2048'])
    for bad in [['--snapshot-xid','379163'],['--volume-block','1'],['--snapshot-xid',str(meta['snapshot_xid']),
                '--id','24','--destination',str(root/'wrong')]]:
        run(binary,'apfs','--source',disk,'--json',*bad,expected=2)
        check(not (root/'wrong').exists(),'unselected live ID')
    check(file_hash(disk)==meta['sha256'] and file_hash(packed)==meta['compressed_sha256'],'sources changed')
    return proof


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',required=True,type=Path)
    with tempfile.TemporaryDirectory() as temp:verify(p.parse_args().binary.resolve(),Path(temp))
