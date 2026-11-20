"""Verify both Windows sector bitmap chunks across 4 GiB and seven NTFS unlinks in each volume."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import tempfile

from e2e import check, run
from e2e_qcow2_backing import checked_import
from e2e_vhd_chain import member
from e2e_vhdx import CORPUS, digest
from e2e_vhdx_chain import MIB, REF, tables, u32, u64


def full_guest(path, archive, spec):
    check(digest(archive) == spec['nonzero_xz_sha256'], archive)
    nonzero = set(spec['nonzero_mib'])
    check(len(nonzero) == len(spec['nonzero_mib']) and sorted(nonzero) == spec['nonzero_mib'], spec['nonzero_mib'])
    blocks = spec['guest_mib_sha256']
    check(len(blocks)*MIB == path.stat().st_size == spec['guest_bytes'], path)
    h = hashlib.sha256(); zero = bytes(MIB)
    with path.open('rb') as guest, lzma.open(archive) as stored:
        for i, expected_sha in enumerate(blocks):
            expected = stored.read(MIB) if i in nonzero else zero
            actual = guest.read(MIB)
            check(len(expected)==MIB and expected==actual and hashlib.sha256(actual).hexdigest()==expected_sha, (path,i))
            h.update(actual)
        check(not stored.read(1) and not guest.read(1), path)
    check(h.hexdigest()==spec['guest_sha256'],path)


def verify(binary, root):
    ref = REF/'boundary-4g'
    manifest = json.loads((ref/'manifest.json').read_text())
    chain = []; out = root/'guest.raw'
    for spec in manifest['members']:
        p = root/spec['filename']
        member(p,spec,ref)
        chain.insert(0,p)
        report = checked_import(binary,chain,out,['--raw-sha256',spec['guest_sha256'],
                                '--raw-bytes',spec['guest_bytes']],kind='vhdx')
        check(report['sha256']==spec['guest_sha256'],report)
        full_guest(out,ref/(p.name+'.nonzero.xz'),spec)
        if len(chain) == 1:
            out.unlink()
    # Inspect the actual Windows-created BAT of the child: one distinct allocated
    # sector bitmap and at least one partial block must exist on each side.
    child = chain[0].read_bytes()
    bat, meta = tables(child)
    block = u32(child, meta['caa16737-fa36-4d43-b3b6-33f0aa44e76b'][0])
    sector = u32(child, meta['8141bf1d-a96f-4709-ba47-f233a8faab5f'][0])
    ratio = (1<<23)*sector//block
    bitmaps = []; partial = []
    for group in range(2):
        start = bat + group*(ratio+1)*8
        off = u64(child,start+8*ratio)
        check(off&7 == 6 and off & ~(MIB-1) != 0, (group,off))
        bitmaps.append(off & ~(MIB-1))
        count=sum(u64(child,start+i*8)&7 == 7 for i in range(ratio))
        check(count,group);partial.append(count)
    check(bitmaps[0]!=bitmaps[1] and ratio*block==1<<32,bitmaps)
    layout = run(binary,'inspect','--source',out,'--json')['layout']
    matches = [r for r in layout['regions'] if r['filesystem']=='ntfs']
    check(sorted(r['offset']//MIB for r in matches) == manifest['partition_mib'],matches)
    # Scan the entire GPT disk. Partition-prefixed candidate IDs distinguish the
    # two deleted copies that share each original name and digest.
    result = {}
    for mode, flags in [('regular', []), ('indexed', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', out, '--work-dir', root/(mode+'-scan'),
                   *flags, '--json', expected=5 if mode == 'regular' else 0, timeout=180)
        if mode == 'regular':
            check(scan['candidates'] == 14 and scan['invalid_records'] == 0 and not scan['cancelled']
                  and any('reserved partition' in w.lower() for w in scan['warnings']), scan)
        else:
            check(scan['index']['candidates'] == 14 and scan['index']['state'] == 'completed', scan)
        candidates = run(binary, 'results', '--session', scan['session'], '--limit', 500, '--json')['candidates']
        owned = {}
        for region in matches:
            prefix = 'p'+str(region['index'])+':'
            selected = [r for r in candidates if r['id'].startswith(prefix) and r['name'] in manifest['originals']]
            check(len(selected)==7 and {r['name'] for r in selected}==manifest['originals'].keys(), selected)
            check(all(r['supported'] and 'deletions/' in r['original_path'] for r in selected), selected)
            for r in selected:
                owned[r['id']] = (str(region['offset']), r['name'])
        ids = [v for cid in owned for v in ('--id', cid)]
        recovered = run(binary, 'recover', '--session', scan['session'], *ids,
                        '--destination', root/(mode+'-files'), '--json', timeout=180)['results']
        check(len(recovered)==14, recovered)
        observed = {}
        for row in recovered:
            offset, name = owned[row['candidate_id']]
            restored = Path(row['path'])
            check(row['status']=='completed' and restored.read_bytes()==(CORPUS/name).read_bytes()
                  and digest(restored)==row['sha256']==manifest['originals'][name], row)
            observed.setdefault(offset, {})[name]=row['sha256']
        check(all(names==manifest['originals'] for names in observed.values()), observed)
        if result:check(result==observed, 'ordinary and indexed recovery disagree')
        result=observed
    return dict(guests=[m['guest_sha256'] for m in manifest['members']],bytes=out.stat().st_size,
                bitmap_offsets=bitmaps,partial_payloads_per_chunk=partial,
                ordinary_scan_skipped_microsoft_reserved_partition=True, indexed_ntfs_volumes=len(matches),
                changed_guest_mib=manifest['changed_guest_mib'],deletions_by_volume=result)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--output',type=Path)
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as root:
        result=verify(a.binary.resolve(),Path(root))
    if a.output:
        a.output.write_text(json.dumps(result,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    print(result['bytes'],'guest bytes,',result['partial_payloads_per_chunk'],
          'partial blocks across two Windows bitmaps, 14 NTFS originals matched',flush=True)
