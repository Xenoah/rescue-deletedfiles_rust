"""Recover real ustar, PAX and GNU archives using the writer's 512-byte record factor."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile

from e2e import check, run
from e2e_extended_filesystems import command

GOLD = Path(__file__).resolve().parents[1] / 'tests/tar'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def verify(binary, root, native=False):
    manifest = json.loads((GOLD/'manifest.json').read_text(encoding='utf-8'))
    proof = {}
    for factor in ([20] if native else [1, 7, 20]):
        folder = root / str(factor)
        folder.mkdir()
        cases = {name: m for name, m in manifest.items() if m['blocking_factor'] == factor}
        files = {name + '.tar': (GOLD/(name + '.tar')).read_bytes() for name in cases}
        lookup = {meta['sha256']: name for name, meta in cases.items()}
        check(all(digest(files[n+'.tar']) == m['sha256'] for n, m in cases.items()), cases)
        if native:
            for name, data in files.items():
                (folder/name).write_bytes(data)
            # FAT keeps the archive's zero-filled record padding in allocated clusters.
            disk = folder / 'deletions.raw'
            with disk.open('wb') as image:
                image.truncate(48 << 20)
            command('mkfs.fat', '-F', '32', '-s', '1', disk)
            for name, data in files.items():
                command('mcopy', '-i', disk, folder/name, '::/'+name)
                readback=folder/('readback-'+name)
                command('mcopy','-i',disk,'::/'+name,readback)
                check(readback.read_bytes()==data, name)
            before_names=command('mdir','-b','-i',disk,'::/').stdout.decode()
            for name in files:
                check(name in before_names,name)
                command('mdel','-i',disk,'::/'+name)
            after_names=command('mdir','-i',disk,'::/').stdout.decode()
            check(all(name not in after_names for name in files),after_names)
        else:
            disk = folder / 'volume.raw'
            data = bytearray(b'\x93' * ((1 << 20) - 105))
            for file in files.values():
                data.extend(file)
                data.extend(b'\xb3' * 1043)
            disk.write_bytes(data)
        before = digest(disk.read_bytes())
        for mode, flags in [('session', []), ('index', ['--indexed'])]:
            args = [] if factor == 1 else ['--tar-blocking-factor', str(factor)]
            scan=run(binary,'scan','--source',disk,'--mode','carve','--extension','tar',
                     '--work-dir',folder/mode,'--json',*args,*flags,timeout=120)
            rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
            check(len(rows)==len(files) and {r['record_sha256'] for r in rows}==set(lookup), rows)
            ids=[v for row in rows for v in ('--id',row['id'])]
            results=run(binary,'recover','--session',scan['session'],*ids,'--destination',folder/(mode+'-out'),'--json')['results']
            check(len(results)==len(files), results)
            for r in results:
                name=lookup[r['sha256']]; path=Path(r['path'])
                check(path.read_bytes()==files[name+'.tar'] and digest(path.read_bytes())==r['sha256'], r)
                with tarfile.open(path) as archive:
                    members={m.name:digest(archive.extractfile(m).read()) for m in archive if m.isfile() or m.islnk() or m.issym()}
                check(members==cases[name]['members'], name)
                if native:command('tar','-tf',path)
                proof[name]=r['sha256']
            print(mode, len(results), 'actual FAT32 TAR deletions' if native else 'TAR files', 'byte-exact; blocking factor', factor, flush=True)
        check(digest(disk.read_bytes())==before,'source changed')
    if not native:
        compact=(GOLD/'compact.tar').read_bytes()
        longname=(GOLD/'gnu-long.tar').read_bytes()
        pax=(GOLD/'pax-unicode.tar').read_bytes()
        def damage(data, offset):
            tmp=bytearray(data);tmp[offset]^=1;return bytes(tmp)
        # Only original-offset recovery is at issue; later surviving members may independently form a TAR prefix.
        for i, (data, factor) in enumerate(((damage(compact,148),1),(damage(compact,512+int(compact[124:136].strip(b"\0 "),8)),1),
                     (compact[:-512],1),(damage(compact,len(compact)-1),1),(compact,20),
                     (damage(longname,512+int(longname[124:136].strip(b'\0 '),8)-1),20),
                     (pax[:512]+b'!'+pax[513:],20))):
            bad=root/f'damaged-{i}.raw';bad.write_bytes(data)
            result=subprocess.run([str(binary),'scan','--source',str(bad),'--mode','carve','--extension','tar',
                                   '--tar-blocking-factor',str(factor),'--work-dir',str(root/f'bad-{i}'),'--json'],capture_output=True,text=True)
            check(result.returncode in (0,5), result.stderr)
            rows=run(binary,'results','--session',json.loads(result.stdout)['session'],'--json')['candidates']
            check(not any(row['record']==0 and row['supported'] for row in rows),(i,rows))
    return proof


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--native',action='store_true')
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:verify(a.binary.resolve(), Path(tmp), a.native)
