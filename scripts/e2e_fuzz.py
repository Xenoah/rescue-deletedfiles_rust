"""Deterministic mutation fuzzing of every parser reachable from the CLI.

Seeded, dependency-free and identical on all native VMs. Mutates synthetic NTFS
(with valid USA fixups re-applied so mutations reach attribute parsing), FAT32,
exFAT and carving images, then runs scan, results, recover and preview. A case
fails on a Rust panic (exit 101), a signal, a timeout, an undocumented exit
code, an output outside the destination, or any change to the source image.
This is bounded regression fuzzing, not coverage-guided fuzzing.
"""
import argparse, hashlib, json, os, platform, random, struct, subprocess, sys, tempfile, time
from pathlib import Path
from e2e import check, inside
import make_fixture
from make_fixture import MFT_LCN, RECORD, SECTOR
from make_fat_fixture import fat32, exfat
import make_carve_fixture

DOCUMENTED = {0, 2, 4, 5, 6, 130}
BOUNDARY = [0, 1, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fffffff, 0x80000000, 0xffffffff]


def call(binary, *args, timeout=60):
    started = time.monotonic()
    try:
        p = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                           encoding='utf-8', errors='replace', timeout=timeout)
    except subprocess.TimeoutExpired:
        raise AssertionError(f'timeout after {timeout}s: {args}')
    check(p.returncode in DOCUMENTED, f'undocumented exit {p.returncode} (101 = panic): {args}\n{p.stderr[-2000:]}')
    check('panicked' not in p.stderr, f'panic message: {args}\n{p.stderr[-2000:]}')
    return p, time.monotonic() - started


def refixup(image, start):
    """Re-apply NTFS update sequence values so a mutated record still passes fixup."""
    usa = struct.unpack_from('<H', image, start + 4)[0]
    if usa + 6 > RECORD:
        return
    value = struct.unpack_from('<H', image, start + usa)[0]
    for i in (1, 2):
        end = start + i * SECTOR - 2
        image[start + usa + i * 2:start + usa + i * 2 + 2] = image[end:end + 2]
        struct.pack_into('<H', image, end, value)


def mutate(rng, data, regions, ntfs=False):
    image = bytearray(data)
    for _ in range(rng.randint(1, 8)):
        low, high = rng.choice(regions)
        at = rng.randrange(low, max(low + 1, high - 4))
        kind = rng.randrange(4)
        if kind == 0:
            image[at] ^= 1 << rng.randrange(8)
        elif kind == 1:
            image[at] = rng.randrange(256)
        elif kind == 2:
            at -= at % 4
            struct.pack_into('<I', image, at, rng.choice(BOUNDARY))
        else:
            struct.pack_into('<H', image, at - at % 2, rng.choice(BOUNDARY) & 0xffff)
    if ntfs:
        mft = MFT_LCN * SECTOR
        for record in range(make_fixture.RECORDS):
            start = mft + record * RECORD
            if image[start:start + 4] == b'FILE' and rng.random() < 0.9:
                refixup(image, start)
    if rng.random() < 0.05:
        image = image[:rng.randrange(SECTOR, len(image))]
    return bytes(image)


def corpora(root):
    out = {}
    ntfs = root / 'ntfs.raw'; make_fixture.build(ntfs)
    mft = MFT_LCN * SECTOR
    out['ntfs'] = (ntfs.read_bytes(), [(0, SECTOR), (mft, mft + make_fixture.RECORDS * RECORD)], True, [])
    f32 = root / 'fat32.raw'; fat32(f32)
    out['fat32'] = (f32.read_bytes(), [(0, 64 * 1024)], False, [])
    ex = root / 'exfat.raw'; exfat(ex)
    out['exfat'] = (ex.read_bytes(), [(0, 64 * 1024)], False, [])
    carve = root / 'carve.raw'; truth = make_carve_fixture.build(carve)
    regions = [(int(k[4:], 16), int(k[4:], 16) + len(v)) for k, v in truth.items()]
    out['carve'] = (carve.read_bytes(), regions, False, ['--mode', 'carve'])
    return out


def main(binary, cases, seed):
    rng = random.Random(seed)
    stats = {'cases': 0, 'scan_exit': {}, 'candidates': 0, 'recovered': 0, 'previews': 0, 'max_seconds': 0.0}
    per_kind = {}
    with tempfile.TemporaryDirectory(prefix='rescue-fuzz-') as temporary:
        root = Path(temporary)
        sources = corpora(root)
        for index in range(cases):
            kind = list(sources)[index % len(sources)]
            data, regions, ntfs, mode = sources[kind]
            case = root / f'case-{index}'; case.mkdir()
            source = case / 'source.raw'; source.write_bytes(mutate(rng, data, regions, ntfs))
            before = hashlib.sha256(source.read_bytes()).hexdigest()
            p, seconds = call(binary, 'scan', '--source', source, *mode, '--work-dir', case / 'work', '--json')
            stats['max_seconds'] = max(stats['max_seconds'], seconds)
            stats['scan_exit'][str(p.returncode)] = stats['scan_exit'].get(str(p.returncode), 0) + 1
            per_kind[kind] = per_kind.get(kind, 0) + 1
            if p.returncode in (0, 5) and p.stdout.strip():
                session = json.loads(p.stdout.strip().splitlines()[-1])['session']
                results, _ = call(binary, 'results', '--session', session, '--limit', '20', '--json')
                if results.returncode == 0:
                    candidates = json.loads(results.stdout)['candidates']
                    stats['candidates'] += len(candidates)
                    ids = [c['id'] for c in candidates if c['supported']][:5]
                    if ids:
                        out = case / 'out'
                        args = [x for i in ids for x in ('--id', i)]
                        recovered, _ = call(binary, 'recover', '--session', session, *args, '--destination', out, '--json')
                        if recovered.returncode == 0:
                            for r in json.loads(recovered.stdout)['results']:
                                if r.get('path'):
                                    check(inside(r['path'], out), f'output escaped destination: {r}')
                                stats['recovered'] += r['status'] == 'completed'
                        prev, _ = call(binary, 'preview', '--session', session, '--id', ids[0], '--destination', case / 'preview', '--json')
                        stats['previews'] += prev.returncode in (0, 6)
            check(hashlib.sha256(source.read_bytes()).hexdigest() == before, f'source changed in case {index}')
            stats['cases'] += 1
    result = {'status': 'passed', 'version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
              'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(), 'platform': platform.platform(),
              'seed': seed, 'checks': stats['cases'], 'per_corpus': per_kind, **stats, 'panics': 0, 'timeouts': 0,
              'source_unchanged': True, 'scope': __doc__.strip()}
    print(json.dumps({k: result[k] for k in ['status', 'checks', 'scan_exit', 'candidates', 'recovered', 'previews', 'max_seconds']}))
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--cases', type=int, default=200)
    p.add_argument('--seed', type=int, default=20261106)
    p.add_argument('--output', type=Path)
    a = p.parse_args()
    report = main(a.binary.resolve(), a.cases, a.seed)
    if a.output:
        a.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
