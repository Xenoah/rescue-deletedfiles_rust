"""Carve AAC-LC ADTS recordings and compare reconstructed bytes, SHA-256, and native PCM."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run
from e2e_extended_filesystems import command, make_deleted

GOLD = Path(__file__).resolve().parents[1] / 'tests/aac'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def samples():
    meta = json.loads((GOLD / 'manifest.json').read_text())
    files = {name + '.aac': (GOLD / (name + '.aac')).read_bytes() for name in meta}
    check(all(digest(data) == meta[name[:-4]]['sha256'] for name, data in files.items()), 'fixture SHA-256')
    return files


def verify(binary, root, native=False):
    files = samples()
    wanted = {digest(data): name for name, data in files.items()}
    native_pcm = {}
    if native:
        for name, data in files.items():
            (root / name).write_bytes(data)
            native_pcm[name] = pcm(root / name)
        disk = make_deleted(root, 'ext4', files)
        deleted = command('fls', '-dFrp', '-f', 'ext4', disk).stdout.decode()
        check(all('\t' + name + '\n' in deleted for name in files), deleted)
    else:
        disk = root / 'recordings.raw'
        source = bytearray(b'\x93' * ((1 << 20) - 4))
        for data in files.values():
            source.extend(data)
            source.extend(b'\xac' * 53)
        disk.write_bytes(source)
    original_disk = digest(disk.read_bytes())
    for label, flags in [('session', []), ('index', ['--indexed'])]:
        report = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'aac',
                     '--work-dir', root / label, '--json', *flags, timeout=120)
        found = run(binary, 'results', '--session', report['session'], '--json')['candidates']
        check(len(found) == len(files) and all(row['supported'] for row in found), found)
        check({r['record_sha256'] for r in found} == set(wanted), found)
        ids = [v for row in found for v in ('--id', row['id'])]
        rows = run(binary, 'recover', '--session', report['session'], *ids, '--destination', root / (label + '-out'), '--json')['results']
        for row in rows:
            path = Path(row['path'])
            name = wanted[row['sha256']]
            check(path.read_bytes() == files[name] and digest(path.read_bytes()) == row['sha256'], name)
            if native:
                check(pcm(path) == native_pcm[name] and len(native_pcm[name]) > 0, name)
        check(len(rows) == len(files), rows)
        print(label, len(rows), 'actual ext4 deletions' if native else 'ADTS recordings', 'match bytes and SHA-256', flush=True)
    check(digest(disk.read_bytes()) == original_disk, 'source changed')
    if not native:
        data = files['stereo-music.aac']
        n = (int.from_bytes(data[3:6], 'big') >> 5) & 8191
        second = (int.from_bytes(data[n+3:n+6], 'big') >> 5) & 8191
        bad_payload = bytearray(data)
        bad_payload[n+7:n+second] = b'\xff' * (second-7)
        for i, bad in enumerate((data[:-2], bytes(bad_payload), data[:n], data[:n+second],
                                 bytes([255, 240]) + data[2:], data + b'\xff\xf1')):
            path = root / f'damaged-{i}.aac'
            path.write_bytes(bad)
            p = subprocess.run([str(binary), 'scan', '--source', str(path), '--mode', 'carve', '--extension', 'aac',
                        '--work-dir', str(root/f'damaged-{i}'), '--json'], capture_output=True, text=True, timeout=45)
            check(p.returncode in (0, 5), p.stderr)
            rows = run(binary, 'results', '--session', json.loads(p.stdout)['session'], '--json')['candidates']
            check(not any(r['record'] == 0 and r['supported'] for r in rows), (i, rows))
        # Two adjoining ADTS streams with the same configuration have no distinguishing delimiter.
        joined = root / 'joined.aac'
        joined.write_bytes(data + data)
        scan = run(binary, 'scan', '--source', joined, '--mode', 'carve', '--extension', 'adts',
                   '--work-dir', root / 'joined-work', '--json')
        row, = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(row['size'] == len(data)*2 and row['record_sha256'] == digest(data+data), row)
    return {name: digest(data) for name, data in files.items()}


def pcm(path):
    return subprocess.check_output(['ffmpeg','-nostdin','-loglevel','error','-i',str(path),
                                    '-f','s16le','-acodec','pcm_s16le','-'])


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--native', action='store_true')
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.native)
