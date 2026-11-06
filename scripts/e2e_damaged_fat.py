"""Damaged USB/SD FAT32 media: one FAT mirror overwritten, cut or unreadable.

Each case damages only one of the two FAT copies of a synthetic volume. Recovery
must use the single copy that gives a loop-free chain (of the recorded length for
files), recover every expected file byte-exact and say so in the scan warnings.
When both copies give different valid chains the volume must still be refused,
because choosing one would be a guess. An unreadable FAT sector in an acquired
image is handled the same way. Truth bytes come from the fixture generator.
"""
import argparse, json, struct, subprocess, tempfile
from pathlib import Path
from e2e import check, run
from e2e_imaging import mark_gap, sha
from make_fat_fixture import fat32


def damage(path, layout, copy, cluster, value):
    with path.open('r+b') as f:
        f.seek(layout['fat'] + copy * layout['fat_sectors'] * 512 + cluster * 4)
        f.write(struct.pack('<I', value))


def recover_all(binary, source, work, out, expected=0):
    scan = run(binary, 'scan', '--source', source, '--work-dir', work, '--json', expected=expected)
    if expected not in (0, 5):
        return scan, None
    candidates = run(binary, 'results', '--session', scan['session'], '--limit', '1000', '--json')['candidates']
    ids = [c['id'] for c in candidates if c['supported']]
    args = [x for i in ids for x in ('--id', i)]
    result = run(binary, 'recover', '--session', scan['session'], *args, '--destination', out, '--json')
    report = run(binary, 'report', '--session', scan['session'], '--json')
    return report, {r['candidate_id']: Path(r['path']).read_bytes() for r in result['results'] if r['status'] == 'completed'}


def main(binary, output=None):
    cases = []
    with tempfile.TemporaryDirectory(prefix='rescue-damaged-fat-') as tmp:
        root = Path(tmp)
        clean = root / 'clean.raw'
        truth, _, layout = fat32(clean)
        _, baseline = recover_all(binary, clean, root / 'w0', root / 'o0')
        check(all(baseline.get(k) == v for k, v in truth.items()), 'baseline recovery')
        scenarios = [
            ('second FAT copy marks the root cluster bad', [(1, 2, 0x0FFFFFF7)]),
            ('first FAT copy cut inside a deleted file chain', [(0, 105, 0)]),
            ('first FAT copy loops the second root cluster', [(0, 8, 8)]),
        ]
        for index, (name, edits) in enumerate(scenarios):
            source = root / f'case{index}.raw'
            source.write_bytes(clean.read_bytes())
            for copy, cluster, value in edits:
                damage(source, layout, copy, cluster, value)
            before = sha(source)
            report, recovered = recover_all(binary, source, root / f'w{index}', root / f'o{index}')
            for key, data in truth.items():
                check(recovered.get(key) == data, f'{name}: {key} not byte-exact')
            check(any('Damaged FAT mirror' in w for w in report['warnings']), f'{name}: copy choice not reported {report["warnings"]}')
            check(sha(source) == before, f'{name}: source changed')
            cases.append(name)
        # Both copies valid but different: refuse rather than guess.
        source = root / 'ambiguous.raw'; source.write_bytes(clean.read_bytes())
        damage(source, layout, 1, 2, 0x0FFFFFFF)
        p = subprocess.run([str(binary), 'scan', '--source', str(source), '--work-dir', str(root / 'wa'), '--json'], capture_output=True, text=True, encoding='utf-8')
        if p.returncode in (0, 5):
            session = json.loads(p.stdout.strip().splitlines()[-1])['session']
            report = run(binary, 'report', '--session', session, '--json')
            check(any('none is chosen' in w for w in report['warnings']) and not any('Damaged FAT mirror' in w for w in report['warnings']), f'ambiguous mirrors were guessed: {report["warnings"]}')
        else:
            check('none is chosen' in p.stderr, p.stderr)
        cases.append('ambiguous valid mirrors are refused, not guessed')
        # Unreadable sector of the first FAT copy in an acquired image.
        # This 32 MiB FAT volume requires over 8,000 durable 4 KiB writes.
        # Windows and macOS Intel runners can exceed the usual 30-second CLI
        # budget; keep the small blocks so the injected gap affects only FAT 1.
        acquired = run(binary, 'image', '--source', clean, '--destination', root / 'image', '--block-size', '4096', '--json', timeout=180)
        image = Path(acquired['image'])
        mark_gap(image, layout['fat'])
        report, recovered = recover_all(binary, image, root / 'wg', root / 'og', expected=5)
        for key, data in truth.items():
            check(recovered.get(key) == data, f'unreadable FAT sector: {key} not byte-exact')
        cases.append('unreadable first-copy FAT sector served by the second copy')
    result = {'status': 'passed', 'version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
              'binary_sha256': sha(binary), 'checks': len(cases), 'cases': cases, 'files_byte_exact': len(truth) * 4, 'source_unchanged': True, 'scope': __doc__.strip()}
    if output:
        Path(output).write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False))
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--binary', type=Path, required=True); p.add_argument('--output', type=Path)
    a = p.parse_args(); main(a.binary.resolve(), a.output)
