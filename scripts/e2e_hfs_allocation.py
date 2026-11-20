"""Recover real Apple HFS Plus/HFSX unlinks and exclude identical files still in use."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import candidate_evidence, check, run
from e2e_apple_partitions import make_image
from e2e_disks import disk_image
from e2e_formats import file_hash
from e2e_imaging import mark_gap

TESTS = Path(__file__).resolve().parents[1] / 'tests'
REFERENCE = TESTS / 'hfsplus'


def scan(binary, image, work, scope=None, indexed=False, extensions='bmp,gif,png,pdf,webp,wav,zip', expected=0, bounds=None):
    args = ['scan', '--source', image, '--mode', 'carve', '--extension', extensions,
            '--work-dir', work, '--json']
    if bounds:
        args += ['--range-start', bounds[0], '--range-end', bounds[1]]
    if scope:
        args += ['--carve-scope', scope]
    if indexed:
        args += ['--indexed']
    session = run(binary, *args, expected=expected, timeout=180)
    rows = candidate_evidence(run(binary, 'results', '--session', session['session'], '--limit', 100, '--json'))
    return session, rows


def restore(binary, image, ref, work, *, shift=0, indexed=False, bounds=None):
    before = file_hash(image)
    session, rows = scan(binary, image, work/'scan', indexed=indexed, bounds=bounds)
    expected = {v['deleted_offset'] + shift: name for name, v in ref.items() if v['deleted_bytes_retained']}
    check({r['record'] for r in rows} == expected.keys(), (rows, session))
    check(all(r['allocation_status'] == 'unallocated' for r in rows), rows)
    names = {r['id']: expected[r['record']] for r in rows}
    outputs = run(binary, 'recover', '--session', session['session'], '--all',
                  '--destination', work/'recovered', '--json', timeout=180)['results']
    check({item['candidate_id'] for item in outputs} == names.keys(), outputs)
    result = {}
    for item in outputs:
        name = names[item['candidate_id']]
        original = (TESTS/'corpus/files'/name).read_bytes()
        output = Path(item['path']).read_bytes()
        check(item['status'] == 'completed' and output == original and len(output) == ref[name]['bytes'], name)
        digest = hashlib.sha256(output).hexdigest()
        check(digest == item['sha256'] == ref[name]['sha256'], item)
        result[name] = digest
    check(before == file_hash(image), 'scan or restore changed source')
    return result


def independent(image, ref):
    listing = subprocess.check_output(['fls', '-r', '-p', '-f', 'hfs', str(image)], text=True)
    paths = {line.split('\t', 1)[1] for line in listing.splitlines() if '\t' in line}
    with image.open('rb') as src:
        src.seek(1024 + 40)
        block_size, = struct.unpack('>I', src.read(4))
    for name, v in ref.items():
        check('live/' + name in paths and 'deleted/' + name not in paths, listing)
        for key, state in [('live_offset', 'Allocated'), ('deleted_offset', 'Not Allocated')]:
            for block in range(v[key]//block_size, (v[key] + v['bytes'] - 1)//block_size + 1):
                stdout = subprocess.check_output(['blkstat', '-f', 'hfs', str(image), str(block)], text=True)
                check(state in stdout.splitlines(), (name, key, block, stdout))


def degraded(binary, root, pristine, ref):
    u32 = lambda buf, at: struct.unpack_from('>I', buf, at)[0]
    header = 1024
    alt = len(pristine) - 1024
    bs = u32(pristine, header + 40)
    bitmap = u32(pristine, header + 128) * bs
    before = u32(pristine, header + 48)
    attrs = u32(pristine, header + 4)
    def word(value):
        return struct.pack('>I', value)
    # Corrupt each independent oracle; unknown fallback is visible and never verified free.
    edits = {
        'mounted': [(header + 4, word(attrs & ~(1 << 8)))],
        'inconsistent_volume': [(header + 4, word(attrs | (1 << 14)))],
        'inconsistent_boot': [(header + 4, word(attrs | (1 << 11)))],
        'bad_count': [(header + 48, word(before - 1))],
        'bad_alternate': [(alt + 40, word(bs * 2))],
        'unallocated_header': [(bitmap, bytes([pristine[bitmap] & 0x7f])), (header + 48, word(before + 1))],
        'missing_overflow': [(pos + 124, word(u32(pristine, pos + 124) + 1)) for pos in (header, alt)],
        'outside_volume': [(pos + 128, word(u32(pristine, header + 44))) for pos in (header, alt)],
        'missing_catalog': [(header + 272, bytes(80))],
        'overlap_header': [(header + 192 + 16, bytes(4))],
        'short_bitmap': [(pos + 112, bytes(8)) for pos in (header, alt)],
    }
    wanted = {ref['sample.png'][key] for key in ('deleted_offset', 'live_offset')}
    def unknown(image, label, expected=0):
        session, rows = scan(binary, image, root/label, extensions='png', expected=expected)
        check({r['record'] for r in rows} == wanted, rows)
        check(all(r['allocation_status'] == 'unknown' for r in rows), rows)
        check(any(w.startswith('ALLOCATION_FALLBACK:') for w in session['warnings']), session)
    image = root/'corrupted.raw'
    for label, mutations in edits.items():
        data = bytearray(pristine)
        for at, payload in mutations:
            data[at:at+len(payload)] = payload
        image.write_bytes(data)
        unknown(image, label)
    image.write_bytes(pristine)
    # The 6478-byte deleted WAV spans two 4096-byte blocks. Reuse only its second block.
    wav = ref['sample.wav']
    block = wav['deleted_offset']//bs + 1
    check(wav['bytes'] > bs and not pristine[bitmap+block//8] & (0x80 >> (block%8)), wav)
    # The primary header and bitmap of this partition lie outside all fingerprint samples.
    wrapper = root/'reuse-gpt.raw'
    ((start, _),) = disk_image(wrapper, [image], sector=4096)
    shift = start * 4096
    stat = wrapper.stat()
    session, (row,) = scan(binary, wrapper, root/'before-reuse', extensions='wav')
    check(row['record'] == shift + wav['deleted_offset'] and row['allocation_status'] == 'unallocated', row)
    with wrapper.open('r+b') as dest:
        dest.seek(shift+bitmap+block//8)
        dest.write(bytes([pristine[bitmap+block//8] | (0x80 >> (block%8))]))
        dest.seek(shift+header+48)
        dest.write(word(before-1))
    os.utime(wrapper, ns=(stat.st_atime_ns, stat.st_mtime_ns))
    changed, rows = scan(binary, wrapper, root/'reuse', extensions='wav')
    def fingerprint(result):
        return json.loads(Path(result['session']).read_text(encoding='utf-8'))['fingerprint']
    check(fingerprint(session) == fingerprint(changed), 'allocation changes must be outside fingerprint samples')
    check(not rows, rows)
    _, rows = scan(binary, wrapper, root/'reuse-all', scope='all', extensions='wav')
    check({r['record'] for r in rows} == {shift + wav['deleted_offset'], shift + wav['live_offset']}, rows)
    failure = run(binary, 'recover', '--session', session['session'], '--id', row['id'],
                  '--destination', root/'stale', '--json', expected=5)
    check(failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(failure), failure)
    failure = run(binary, 'recover', '--stream', '--session', session['session'], '--id', row['id'],
                  '--destination', root/'changed-stream', '--json', expected=5)
    check(failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(failure), failure)
    wrapper.unlink()
    # An unreadable allocation file cannot supply free-space evidence.
    image.write_bytes(pristine)
    capture = run(binary, 'image', '--source', image, '--destination', root/'acquired',
                  '--block-size', 4096, '--json', timeout=180)
    mapped = Path(capture['image'])
    mark_gap(mapped, bitmap)
    unknown(mapped, 'unreadable', expected=5)
    return sorted([*edits, 'unreadable', 'partially_reused_file', 'stale_session'])


def journals(binary, root, pristine, ref):
    # Use Apple's captured journal fields to exercise both byte orders and isolate
    # each validation failure. Keep checksum valid for all semantic mutations.
    bs = struct.unpack_from('>I', pristine, 1024+40)[0]
    info = struct.unpack_from('>I', pristine, 1024+12)[0] * bs
    at, size = struct.unpack_from('>QQ', pristine, info+36)
    layout = 'IIQQQIII'
    endian = '<' if pristine[at:at+4] == b'xLNJ' else '>'
    fields = list(struct.unpack_from(endian + layout, pristine, at))
    check(fields[0] == 0x4a4e4c78 and fields[1] == 0x12345678 and fields[2] == fields[3], fields)

    def encoded(order, **updates):
        values = fields.copy()
        for key, value in updates.items():
            values[{'start':2, 'end':3, 'size':4, 'sector':7}[key]] = value
        values[6] = 0
        header = struct.pack(order + layout, *values)
        checksum = 0
        for octet in header:
            checksum = ((checksum << 8) ^ (checksum + octet)) & 0xffffffff
        values[6] = checksum ^ 0xffffffff
        return struct.pack(order + layout, *values)
    check(encoded(endian) == pristine[at:at+44], 'independent Apple journal checksum')
    image = root/'journal-controls.raw'
    image.write_bytes(pristine)
    png = ref['sample.png']
    def change(offset, data):
        with image.open('r+b') as out:
            out.seek(offset)
            out.write(data)
    other = '>' if endian == '<' else '<'
    change(at, encoded(other))
    exact = restore(binary, image, ref, root/'other-journal-endian', indexed=True)
    check(len(exact) == 7, exact)
    changes = {
        'journal_external': (info, struct.pack('>I', 2)),
        'journal_needs_init': (info, struct.pack('>I', 5)),
        'journal_out_of_bounds': (info+36, struct.pack('>Q', len(pristine))),
        'journal_signature': (at, b'JUNK'),
        'journal_endian': (at+4, bytes(4)),
        'journal_checksum': (at+36, bytes(4)),
        'journal_pending': (at, encoded(endian, end=fields[7] if fields[2] != fields[7] else fields[7]*2)),
        'journal_size_mismatch': (at, encoded(endian, size=size+bs)),
        'journal_bad_sector': (at, encoded(endian, sector=0)),
        'journal_bad_cursor': (at, encoded(endian, start=size+512, end=size+512)),
        'journal_alt_disabled': (len(pristine)-1024+4,
                                 struct.pack('>I', struct.unpack_from('>I', pristine, 1024+4)[0] & ~(1<<13))),
    }
    for label, (offset, value) in changes.items():
        image.write_bytes(pristine)
        change(offset, value)
        session, rows = scan(binary, image, root/label, extensions='png')
        check({r['record'] for r in rows} == {png['deleted_offset'], png['live_offset']}, rows)
        check(all(r['allocation_status'] == 'unknown' for r in rows), rows)
        check(any(w.startswith('ALLOCATION_FALLBACK:') for w in session['warnings']), session)
    image.unlink()
    return sorted(changes)


def verify(binary, root, *, tsk=False):
    manifest = json.loads((REFERENCE/'manifest.json').read_text())
    manifest.update(json.loads((REFERENCE/'journaled.json').read_text()))
    result = {}
    for case in ('hfsplus', 'hfsx', 'hfsplus-journaled', 'hfsx-journaled'):
        ref = manifest[case]
        image = root/(case+'.raw')
        data = lzma.decompress((REFERENCE/(case+'.raw.xz')).read_bytes())
        check(len(data) == ref['bytes'] and hashlib.sha256(data).hexdigest() == ref['disk_sha256'], case)
        image.write_bytes(data)
        files = ref['originals']
        check(len(files) == 7 and all(v['deleted_bytes_retained'] for v in files.values()), case)
        if tsk:
            independent(image, files)
        _, rows = scan(binary, image, root/(case+'-control'), scope='all')
        check({r['record'] for r in rows} == {f[k] for f in files.values() for k in ('live_offset','deleted_offset')}, rows)
        exact = restore(binary, image, files, root/(case+'-ordinary'))
        check(restore(binary, image, files, root/(case+'-indexed'), indexed=True) == exact, case)
        if case.startswith('hfsplus'):
            wrapper = root/'apm.raw'
            make_image(wrapper, 512, image, kind=b'Apple_HFS')
            shift = 64*512
        else:
            wrapper = root/'gpt.raw'
            ((start, _),) = disk_image(wrapper, [image], sector=4096)
            shift = start*4096
        check(restore(binary, wrapper, files, root/(case+'-partitioned'), shift=shift, indexed=True) == exact, case)
        wrapper.unlink()
        result[case] = {'disk_sha256': ref['disk_sha256'], 'restored_sha256': exact, 'excluded_live': 7}
        print(case, '7 native Apple unlinks matched original bytes and SHA-256; 7 live controls excluded', flush=True)
        if case == 'hfsplus':
            result['degraded'] = degraded(binary, root, data, files)
        if case == 'hfsplus-journaled':
            result['journal_degraded'] = journals(binary, root, data, files)
        image.unlink()
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--tsk', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        proof = verify(args.binary.resolve(), Path(scratch), tsk=args.tsk)
    if args.output:
        args.output.write_text(json.dumps(proof, indent=2, sort_keys=True)+'\n', encoding='utf-8')
