"""Compare complete carved archives with their input and optionally inspect their members."""
import hashlib
from pathlib import Path

from e2e import check, run


def digest(data):
    return hashlib.sha256(data).hexdigest()


def recover_archives(binary, root, disk, files, extension, inspect=None):
    originals = {digest(data): name for name, data in files.items()}
    check(len(originals) == len(files), 'duplicate archive reference')
    before = digest(disk.read_bytes())
    for label, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', extension,
                   '--work-dir', root / label, '--json', *flags, timeout=180)
        rows = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(rows) == len(files) and {r['record_sha256'] for r in rows} == set(originals), rows)
        ids = [arg for row in rows for arg in ('--id', row['id'])]
        results = run(binary, 'recover', '--session', scan['session'], *ids,
                      '--destination', root / (label + '-out'), '--json')['results']
        check(len(results) == len(files), results)
        for row in results:
            path = Path(row['path'])
            original = originals[row['sha256']]
            actual = path.read_bytes()
            check(actual == files[original] and digest(actual) == row['sha256'], original)
            if inspect:
                inspect(path, original)
        print(label, len(rows), extension, 'files restored byte-for-byte with matching SHA-256', flush=True)
    check(digest(disk.read_bytes()) == before, 'source changed')
    return {name: h for h, name in originals.items()}
