"""Verify raw CD-ROM tracks against a real ISO with seven files removed in its second session."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run
from e2e_udif import restore_files

GOLD = Path(__file__).resolve().parents[1] / 'tests/optical'
ORIGINALS = GOLD.parent / 'corpus/files'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def external(track, logical, label, root, expected):
    cue = root / 'image.cue'
    mode = 'MODE1' if label == 'mode1' else 'MODE2'
    cue.write_text(f'FILE "{track.name}" BINARY\n  TRACK 01 {mode}/2352\n    INDEX 01 00:00:00\n')
    subprocess.run(['bchunk', track, cue, root / 'reference'], check=True, capture_output=True)
    check((root / 'reference01.iso').read_bytes() == logical.read_bytes(), 'independent bchunk')
    def xorriso(*args):
        return subprocess.run(['xorriso', *args], capture_output=True, check=True, timeout=60)
    # Only Current exists at the latest root. The first session still holds the removed directory.
    live = xorriso('-indev', logical, '-ls', '/').stdout.decode()
    check("'Current'" in live and "'Deleted'" not in live, live)
    older = root / 'previous'
    xorriso('-load', 'session', '1', '-indev', logical, '-osirrox', 'on', '-extract', '/Deleted', older)
    check({f.name for f in older.iterdir()} == set(expected), 'historical directory')
    for name, digest in expected.items():
        data = (older / name).read_bytes()
        check(data == (ORIGINALS / name).read_bytes() and sha(data) == digest, name)


def verify(binary, root, independent=False):
    evidence = json.loads((GOLD / 'manifest.json').read_text())
    originals = {sha((ORIGINALS / name).read_bytes()): (ORIGINALS / name).read_bytes() for name in evidence['files']}
    check(set(originals) == set(evidence['files'].values()), 'original corpus hashes')
    tracks = {}
    results = {}
    for label, track_sha in evidence['tracks'].items():
        data = lzma.decompress((GOLD / (label + '.bin.xz')).read_bytes())
        check(sha(data) == track_sha, label)
        tracks[label] = data
        for stride in [2352, 2448]:
            folder = root / f'{label}-{stride}'
            folder.mkdir()
            track = folder / 'track.bin'
            logical = folder / 'restored.iso'
            # Deliberately nonzero subchannel bytes must never enter user data.
            raw = data if stride == 2352 else b''.join(data[i:i+2352] + bytes([1 + i//2352 % 254])*96 for i in range(0,len(data),2352))
            track.write_bytes(raw)
            options = [] if stride == 2352 else ['--image-type','cd-raw']
            report = run(binary, 'import-image', '--source', track, '--output', logical, '--json', *options)
            check(sha(logical.read_bytes()) == report['sha256'] == evidence['logical_sha256'], (label,stride))
            check(logical.stat().st_size == report['bytes'] == evidence['logical_bytes'], 'logical size')
            restore_files(binary, logical, folder, evidence['files'])
            outputs = [p.read_bytes() for p in (folder/'files').rglob('*') if p.is_file() and p.name != 'rescue-report.json']
            matched = {sha(b) for b in outputs if b == originals.get(sha(b))}
            check(matched == originals.keys(), (label,stride,matched))
            if independent and stride == 2352:
                external(track,logical,label,folder,evidence['files'])
            check(track.read_bytes() == raw, 'source changed')
            results[folder.name] = len(matched)
            print(folder.name,len(matched),'removed files, all original bytes and SHA-256 matched',flush=True)

    mode1 = tracks['mode1']
    xa = tracks['xa-form1']
    def flip(data, at):
        altered = bytearray(data)
        altered[at] ^= 1
        return bytes(altered)
    cases = {
        'truncated': mode1[:-1], 'audio': bytes(2352), 'short': b'CD',
        'bad-sync': flip(mode1,2352*8), 'bad-frame-bcd': mode1[:12]+b'\xfa'+mode1[13:],
        'wrong-address': flip(mode1,2352*7+14), 'wrong-mode': flip(mode1,2352*5+15),
        'reserved-mode1': flip(mode1,2068), 'payload': flip(mode1,1064),
        'edc1': flip(mode1,2065), 'p': flip(mode1,2111), 'q': flip(mode1,2305),
        'xa-duplicate': flip(xa,20), 'xa-edc': flip(xa,2072), 'xa-p': flip(xa,2077),
        'xa-q': flip(xa,2321), 'form2': xa[:18]+b'('+xa[19:22]+b'('+xa[23:],
        'missing-sector': mode1[:2352*11]+mode1[2352*12:],
    }
    for label, data in cases.items():
        track = root / (label + '.bin')
        track.write_bytes(data)
        output = root / 'bad.iso'
        run(binary,'import-image','--image-type','cd-raw','--source',track,'--output',output,'--json',expected=4)
        check(not output.exists() and track.read_bytes() == data, label)
    print(len(cases),'unsupported or damaged tracks refused',flush=True)
    return results


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--independent', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as folder:
        verify(args.binary.resolve(),Path(folder),args.independent)
