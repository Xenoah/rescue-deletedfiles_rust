"""Restore seven actual ext4 unlinks whose 16 or 17 extents do not fit inside the inode."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import struct
import tempfile

from e2e import check,run
from e2e_btrfs_snapshots import crc32c,sha
from ext4_sparse_samples import samples

GOLD = Path(__file__).resolve().parents[1]/'tests/ext4-journal-extents'


def verify(binary, root):
    originals=samples()
    expected={n:hashlib.sha256(data).hexdigest() for n,data in originals.items()}
    proof={}
    for size in (1024,4096):
        fixture=GOLD/str(size)
        meta=json.loads((fixture/'manifest.json').read_text())
        disk=root/f'{size}.raw'
        packed=fixture/'volume.raw.xz'
        check(sha(packed)==meta['compressed_sha256'] and expected==meta['originals'],meta)
        with lzma.open(packed,'rb') as f,disk.open('wb') as out:
            shutil.copyfileobj(f,out)
        check(sha(disk)==meta['sha256'] and min(meta['allocated_extents'].values())>=16,meta)
        base=['ext4-journal','--source',disk,'--json']
        rows=run(binary,*base)['candidates']
        check(len(rows)==len(originals),rows)
        observed={}
        for row in rows:
            name,=row['historical_names']
            n=name['name']
            check(row['inode']==meta['unlinked_inodes'][n] and bytes.fromhex(name['name_hex'])==n.encode(),row)
            path=root/(str(size)+'-'+n)
            res=run(binary,*base,'--inode',str(row['inode']),'--sha256',expected[n],'--output',path)
            check(path.read_bytes()==originals[n] and sha(path)==res['saved']['sha256']==expected[n],res)
            observed[n]=res['saved']['sha256']
        check(observed==expected,observed)
        proof[str(size)]=observed
        with disk.open('r+b') as f:
            f.seek(1024+104);uuid=f.read(16)
            leaf_offset=meta['tree_payload_byte'];desc_offset=meta['tree_descriptor_byte']
            f.seek(leaf_offset);leaf=f.read(size)
            f.seek(desc_offset);desc=f.read(size)
            damaged=bytearray(leaf);damaged[-1]^=1
            repaired=bytearray(desc)
            def jcrc(b):return crc32c(b)^0xffffffff
            struct.pack_into('>I',repaired,meta['tree_tag_offset']+12,jcrc(uuid+desc[8:12]+damaged))
            repaired[-4:]=bytes(4)
            struct.pack_into('>I',repaired,size-4,jcrc(uuid+repaired))
            f.seek(leaf_offset);f.write(damaged)
            f.seek(desc_offset);f.write(repaired);f.flush()
            error=run(binary,*base,expected=4)
            check('extent tree checksum' in str(error),error)
            f.seek(leaf_offset);f.write(leaf)
            # Remove the earlier descriptor; no later inode may borrow unpublished data from that tree.
            f.seek(desc_offset);f.write(bytes(4));f.flush()
            check(not run(binary,*base)['candidates'],'missing journal dependency supplied a candidate')
            f.seek(desc_offset);f.write(desc)
        check(sha(disk)==meta['sha256'],'fixture changed')
        print(size,'all seven real sparse files restored with 16–17 extents; missing journal tree and damaged extent checksum refused',flush=True)
    return proof


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:verify(args.binary.resolve(),Path(tmp))
