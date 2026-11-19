"""Unlink ten real audio and font files in ext4 and carve their contents back."""
import argparse
from pathlib import Path
import tempfile

from e2e import check, run
from e2e_extended_filesystems import command, make_deleted, sha

TESTS = Path(__file__).resolve().parents[1]/'tests'


def verify(binary, root):
    paths = [TESTS / 'aiff' / name for name in ('pcm_s16be.aiff', 'pcm_s24be.aiff', 'pcm_f32be.aifc', 'pcm_alaw.aifc')]
    paths += [TESTS / 'fonts' / name for name in ('ttf-1.ttf', 'ttf-5.ttf', 'otf-1.otf', 'otf-5.otf', 'ttf-3.woff', 'otf-5.woff')]
    originals = {p.name:p.read_bytes() for p in paths}
    truth = {sha(data):name for name,data in originals.items()}
    for name, data in originals.items():
        (root/name).write_bytes(data)
    disk = make_deleted(root,'ext4',originals)
    before = sha(disk.read_bytes())
    deleted = command('fls','-dFrp','-f','ext4',disk).stdout.decode()
    check(all(('\t'+name+'\n') in deleted for name in originals),deleted)
    for label, flags in [('session',[]), ('index',['--indexed'])]:
        args=[arg for ext in ('aiff','ttf','otf','woff') for arg in ('--extension',ext)]
        scan=run(binary,'scan','--mode','carve','--source',disk,'--work-dir',root/label,'--json',*args,*flags)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        found=[row for row in rows if row['supported']]
        check(len(found)==len(originals) and {r['record_sha256'] for r in found}==truth.keys(),found)
        selection=[arg for row in found for arg in ('--id',row['id'])]
        results=run(binary,'recover','--session',scan['session'],'--destination',root/(label+'-output'),'--json',*selection)['results']
        check(len(results)==len(originals),results)
        for row in results:
            name=truth[row['sha256']]
            check(Path(row['path']).read_bytes()==originals[name],row)
        print(label,len(results),'actual ext4 deletions byte-exact',flush=True)
    check(sha(disk.read_bytes())==before,'source changed')
    return {name:sha(data) for name,data in originals.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as temp:
        verify(p.parse_args().binary.resolve(),Path(temp))
