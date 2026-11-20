"""Compare a sparse GPT disk byte-for-byte and restore actual ext4 unlinks across 4 GiB."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run
from e2e_qed import CORPUS, GOLD, TESTS
from e2e_qcow2_backing import checked_import

FOUR_GIB = 1 << 32


def compare(original, actual):
    digest = hashlib.sha256()
    with original.open('rb') as ref, actual.open('rb') as tested:
        while chunk := ref.read(4 << 20):
            check(tested.read(len(chunk)) == chunk, f'logical QED data differs at block ending at {ref.tell()}')
            digest.update(chunk)
        check(not tested.read(1), 'extra bytes at end of disk')
    return digest.hexdigest()


def verify(binary, work):
    folder = GOLD/'gpt-4g'
    meta = json.loads((folder/'manifest.json').read_text(encoding='utf-8'))
    base = TESTS/'lvm2-snapshots/exhausted-store'
    proof = json.loads((base/'manifest.json').read_text(encoding='utf-8'))['logical']['deleted']
    volume = lzma.decompress((base/'deleted.raw.xz').read_bytes())
    check(hashlib.sha256(volume).hexdigest() == meta['partition_sha256'] == proof['volume_sha256'], meta)
    size, start = meta['bytes'], meta['offset']
    image = work/'disk.qed'
    image.write_bytes(lzma.decompress((folder/'disk.qed.xz').read_bytes()))
    check(hashlib.sha256(image.read_bytes()).hexdigest() == meta['qed_sha256'], meta)
    head, tail = [lzma.decompress((folder/(n+'.raw.xz')).read_bytes()) for n in ('head','tail')]
    expected = work/'original.raw'
    with expected.open('wb') as f:
        f.write(head)
        f.seek(start)
        f.write(volume)
        f.seek(size-len(tail))
        f.write(tail)
    raw = work/'converted.raw'
    results = checked_import(binary, [image], raw, ['--raw-bytes',size,'--raw-sha256',meta['logical_sha256']],kind='qed')
    check(results['bytes'] == size and compare(expected,raw) == results['sha256'] == meta['logical_sha256'],results)
    subprocess.run(['qemu-img','compare','-q','-f','qed','-F','raw',image,raw], check=True)
    listing = subprocess.check_output(['mmls','-t','gpt',raw],text=True)
    parts = [r.split() for r in listing.splitlines() if r.rstrip().endswith('recovery')]
    check(len(parts)==1 and int(parts[0][2]) == start//512 and int(parts[0][4])*512 == len(volume),parts)
    truth = {h:n for n,h in proof['matched'].items()}
    recovered = {}
    for indexed in (False,True):
        dest = work/('index' if indexed else 'ordinary')
        scan = run(binary,'scan','--source',raw,'--mode','carve','--carve-scope','all','--range-start',start,
                   '--range-end',start+len(volume),'--work-dir',dest,'--json',*(['--indexed'] if indexed else []))
        candidates = run(binary,'results','--session',scan['session'],'--json')['candidates']
        chosen = [c for c in candidates if c['supported'] and c['record_sha256'] in truth]
        check(len(chosen)==len(truth) and {c['record_sha256'] for c in chosen}==set(truth),chosen)
        args = [a for c in chosen for a in ('--id',c['id'])]
        saved = run(binary,'recover','--session',scan['session'],*args,'--destination',dest/'out','--json')['results']
        check(len(saved)==len(truth),saved)
        positions = {c['record_sha256']:c['record'] for c in chosen}
        result = {}
        for row in saved:
            n = truth[row['sha256']]
            data = Path(row['path']).read_bytes()
            check(data==(CORPUS/n).read_bytes() and hashlib.sha256(data).hexdigest()==row['sha256'],row)
            result[n]=dict(offset=positions[row['sha256']],bytes=len(data),sha256=row['sha256'])
        check(min(positions.values())<FOUR_GIB<max(positions.values()),positions)
        check(not recovered or result==recovered,result)
        recovered=result
    return dict(bytes=size, logical_sha256=results['sha256'], partition_offset=start, deleted=recovered)


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--output',type=Path)
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        result=verify(a.binary.resolve(),Path(temporary))
    if a.output:
        a.output.write_text(json.dumps(result,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    print(result['bytes'],'logical bytes matched,',len(result['deleted']),'actual ext4 files restored across 4 GiB')
