"""Restore real kernel-unlinked files from 1 KiB and 4 KiB ext4 JBD2 volumes."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

from btrfs_large_samples import samples
from e2e import run, check
from e2e_btrfs_extents import flip
from e2e_btrfs_snapshots import sha

GOLD = Path(__file__).resolve().parents[1]/'tests/ext4-journal-reference'


def verify(binary, root, independent=False):
    originals = samples()
    expected = {n: hashlib.sha256(b).hexdigest() for n,b in originals.items()}
    proof = {}
    for block in (1024, 4096):
        fixture = GOLD/str(block)
        meta = json.loads((fixture/'manifest.json').read_text())
        packed = fixture/'volume.raw.xz'
        check(expected == meta['originals'] and sha(packed) == meta['compressed_sha256'], meta)
        folder = root/str(block)
        folder.mkdir()
        disk = folder/'volume.raw'
        with lzma.open(packed, 'rb') as src,disk.open('wb') as dst:
            shutil.copyfileobj(src,dst)
        check(sha(disk) == meta['sha256'], disk)
        base = ['ext4-journal', '--source', disk, '--json']
        rows = run(binary, *base)['candidates']
        ids = meta['unlinked_inodes']
        check({r['inode'] for r in rows} == set(ids.values()), rows)
        check(all(r['journal_sequence'] == meta['prior_sequence'] for r in rows), rows)
        matches = {}
        for name,data in originals.items():
            args = base + ['--inode', str(ids[name])]
            listed, = run(binary, *args)['candidates']
            check(listed['size'] == len(data), listed)
            for mode,options in [('known',['--sha256',expected[name]]),('metadata',['--allow-unverified-content'])]:
                output=folder/(mode+'-'+name)
                result=run(binary,*args,'--output',output,*options)
                check(result['original_sha256_matched'] == (mode=='known'),result)
                check(output.read_bytes()==data and expected[name]==sha(output)==result['saved']['sha256'],result)
            matches[name]=expected[name]
        proof[str(block)] = matches
        args = base+['--inode',str(ids['log.txt'])]
        rejected=folder/'rejected'
        run(binary,*args,'--output',rejected,expected=2)
        run(binary,*args,'--output',rejected,'--sha256','f'*64,expected=4)
        run(binary,*args,'--output',folder/'known-log.txt','--sha256',expected['log.txt'],expected=2)
        check(sha(folder/'known-log.txt')==expected['log.txt'] and not rejected.exists(),'failed restore published output')
        run(binary,*base,'--inode',str(meta['parent_inode']),expected=2)
        for field,delta in [('journal_descriptor_byte',100),('journal_inode_payload_byte',12),('journal_commit_byte',16),
                           ('journal_superblock_byte',252)]:
            flip(disk,meta[field]+delta)
            run(binary,*args,'--output',rejected,'--sha256',expected['log.txt'],expected=4)
            check(not rejected.exists(),field)
            flip(disk,meta[field]+delta)
        flip(disk,1024+1020)
        run(binary,*base,expected=4)
        flip(disk,1024+1020)
        at=meta['first_data_block']*block
        flip(disk,at)
        run(binary,*args,'--output',rejected,'--sha256',expected['log.txt'],expected=4)
        altered=folder/'changed-free-sector'
        result=run(binary,*args,'--output',altered,'--allow-unverified-content')
        check(not result['original_sha256_matched'] and altered.read_bytes()!=originals['log.txt'],'ext4 content uncertainty')
        flip(disk,at)
        check(not rejected.exists() and sha(disk)==meta['sha256'],'source changed')
        if independent:
            # libext2fs updates the real allocation bitmap and its checksum.
            subprocess.run(['debugfs','-w','-R','setb '+str(meta['first_data_block']),str(disk)],check=True,capture_output=True)
            denied=run(binary,*args,'--output',rejected,'--sha256',expected['log.txt'],expected=4)
            check('allocated' in str(denied) and not rejected.exists(),'claimed extent not refused')
        print(block,'seven actual unlinks recovered with a known digest and separately with explicit unverified content; damaged metadata and wrong digest refused',flush=True)
    return proof


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--independent',action='store_true')
    options=parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(options.binary.resolve(),Path(tmp),options.independent)
