"""Verify PhotoRec's exported bytes independently of its file names and detection labels."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
from e2e import check, run
from e2e_file_structures import CORPUS


def verify(binary, root):
    image = root / 'without-metadata.img'
    manifest = json.loads((CORPUS / 'manifest.json').read_text(encoding='utf-8'))
    originals = {}
    contents = bytearray(1024 * 1024)
    for i, row in enumerate(sorted(manifest['files'], key=lambda r: r['file'])):
        data = (CORPUS / row['file']).read_bytes()
        check(hashlib.sha256(data).hexdigest() == row['sha256'], row)
        pos = (i + 1) * 24576
        contents[pos:pos+len(data)] = data
        originals[row['sha256']] = row
    image.write_bytes(contents)
    destination = root / 'results'
    destination.mkdir()
    marker = destination / 'keep'
    marker.write_text('existing output', encoding='utf-8')
    directories = set()
    for flags in [[], ['--all-families']]:
        result = run(binary, 'photorec', '--source', image, '--destination', destination,
                     '--block-size', '512', '--json', *flags)
        folder = Path(result['directory'])
        rows = [json.loads(line) for line in Path(result['manifest']).read_text(encoding='utf-8').splitlines()]
        check(len(rows) == result['files'] and result['bytes'] == sum(r['bytes'] for r in rows), result)
        check('unverified' in result['evidence'] and result['all_families'] == bool(flags), result)
        matches = set()
        for row in rows:
            path = folder / row['path']
            check(path.resolve().is_relative_to(folder.resolve()), row)
            data = path.read_bytes()
            check(len(data) == row['bytes'] and hashlib.sha256(data).hexdigest() == row['sha256'], row)
            reference = originals.get(row['sha256'])
            if reference:
                check((CORPUS / reference['file']).read_bytes() == data, reference)
                matches.add(reference['extension'])
        check({'apng', 'ico', 'mp3', 'xml', 'xz'} <= matches, matches)
        check(folder not in directories and folder.parent.samefile(destination), 'output collision')
        directories.add(folder)
        print('all families' if flags else 'default families', len(matches), 'distinct matching extensions;',
              len(rows), 'candidate outputs;', sorted(matches), flush=True)
    check(image.read_bytes() == contents and marker.read_text() == 'existing output', 'source or output changed')
    # A Rescue read map must never be flattened by an external raw scanner.
    Path(str(image) + '.rmap').write_bytes(b'read-map marker')
    refused = root / 'mapped-output'
    run(binary, 'photorec', '--source', image, '--destination', refused, '--json', expected=2)
    check(not refused.exists(), 'destination created for refused source')
    if os.name == 'posix':
        large = root / 'cancel.img'
        with large.open('wb') as stream:
            stream.truncate(512 * 1024 * 1024)
        dest = root / 'cancelled'
        proc = subprocess.Popen([str(binary), 'photorec', '--source', str(large), '--destination', str(dest),
                                 '--json'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline and proc.poll() is None:
                if dest.is_dir() and any(dest.glob('*/photorec.log')):
                    proc.send_signal(signal.SIGINT)
                    break
                time.sleep(.005)
            proc.communicate(timeout=15)
            check(proc.returncode == 130 and not list(dest.iterdir()), 'cancel must stop the reader and clean up')
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate()


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    with tempfile.TemporaryDirectory() as tmp:
        verify(p.parse_args().binary.resolve(), Path(tmp))
