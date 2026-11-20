"""Restore actual Linux XFS unlinks and exclude their still-allocated identical copies."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import lzma
import os
from pathlib import Path
import re
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_btrfs_snapshots import crc32c
from e2e_disks import disk_image
from e2e_formats import file_hash
from e2e_hfs_allocation import restore, scan

REFERENCE = Path(__file__).resolve().parents[1] / 'tests/xfs-allocation'


def expand(archive, image, ref):
    check(file_hash(archive) == ref['xz_sha256'], archive)
    digest = hashlib.sha256()
    with lzma.open(archive) as source, image.open('xb') as target:
        while data := source.read(1 << 20):
            digest.update(data)
            if data.count(0) == len(data):
                target.seek(len(data), 1)
            else:
                target.write(data)
        check(target.tell() == ref['guest_bytes'], image)
        target.truncate()
    check(digest.hexdigest() == ref['guest_sha256'], image)


def independent(image, ref):
    subprocess.run(['xfs_repair', '-n', '-m', '256', image], check=True, capture_output=True)
    journal = subprocess.check_output(['xfs_logprint', '-t', image], text=True)
    check('<CLEAN>' in journal, journal)
    dump = subprocess.check_output(['xfs_db', '-r', '-c', 'freesp -d', image], text=True)
    rows = [list(map(int, line.split())) for line in dump.splitlines()
            if re.fullmatch(r'\s*\d+\s+\d+\s+\d+\s*', line)]
    check(sorted(rows) == sorted(ref['xfs_db_free_extents']), 'xfs_db free extents')
    bs = ref['block_bytes']
    for name, record in ref['files'].items():
        for key, expected in [('deleted_offset', True), ('live_offset', False)]:
            for block in range(record[key] // bs, (record[key] + record['bytes'] - 1) // bs + 1):
                ag, local = divmod(block, ref['agblocks'])
                actual = any(n == ag and start <= local < start + length for n, start, length in rows)
                check(actual == expected, (name, key, block))


def be32(data, at):
    return struct.unpack_from('>I', data, at)[0]


def read_at(image, at, size):
    with image.open('rb') as source:
        source.seek(at)
        data = source.read(size)
    check(len(data) == size, (at, size))
    return data


def write_at(image, at, data):
    with image.open('r+b') as target:
        target.seek(at)
        target.write(data)


def amended(original, crc_field, fields):
    data = bytearray(original)
    for offset, fmt, value in fields:
        struct.pack_into(fmt, data, offset, value)
    if crc_field is not None:
        struct.pack_into('<I', data, crc_field, 0)
        struct.pack_into('<I', data, crc_field, crc32c(data))
    return data


@contextmanager
def changed(image, edits):
    originals = [(at, read_at(image, at, len(data))) for at, data in edits]
    try:
        for at, data in edits:
            write_at(image, at, data)
        yield
    finally:
        for at, original in reversed(originals):
            write_at(image, at, original)


def bounds(files, field, shift=0):
    return (shift + min(v[field] for v in files.values()),
            shift + max(v[field] + v['bytes'] for v in files.values()))


def controls(binary, root, image, ref):
    files, bs, sector = ref['files'], ref['block_bytes'], ref['sector_bytes']
    sb = read_at(image, 0, sector)
    agf = read_at(image, sector, sector)
    fl = read_at(image, 3 * sector, sector)
    bno_at, cnt_at = (be32(agf, i) * bs for i in (16, 20))
    bno, cnt = (read_at(image, at, bs) for at in (bno_at, cnt_at))
    fsb, = struct.unpack_from('>Q', sb, 48)
    aglog = sb[124]
    log_at = ((fsb >> aglog) * ref['agblocks'] + (fsb & ((1 << aglog) - 1))) * bs
    match = re.search(r'head: (\d+)', '\n'.join(ref['journal_report']))
    head = int(match[1])
    # This 512-sector fixture has a one-header, one-sector unmount record.
    last = log_at + (head - 2) * 512
    header = read_at(image, last, 512)
    check(header[:4] == bytes.fromhex('feedbabe') and be32(header, 12) == 512, header[:44])
    edits = {
        'superblock_crc': [(224, bytes([sb[224] ^ 1]))],
        'needs_repair': [(0, amended(sb, 224, [(216, '>I', be32(sb, 216) | 16)]))],
        'external_log': [(0, amended(sb, 224, [(48, '>Q', 0)]))],
        'realtime': [(0, amended(sb, 224, [(16, '>Q', 10)]))],
        'free_total': [(0, amended(sb, 224, [(144, '>Q', int.from_bytes(sb[144:152], 'big') - 1)]))],
        'agf_checksum': [(sector + 216, bytes([agf[216] ^ 1]))],
        'group_free_total': [(sector, amended(agf, 216, [(52, '>I', be32(agf, 52) - 1)]))],
        'agfl_reused_metadata': [(3 * sector, amended(fl, 32, [(36 + be32(agf, 40)*4, '>I', be32(agf, 16))]))],
        'tree_checksum': [(cnt_at + 52, bytes([cnt[52] ^ 1]))],
        'tree_wrong_owner': [(bno_at, amended(bno, 52, [(48, '>I', 1)]))],
        'tree_newer_than_log': [(bno_at, amended(bno, 52, [(24, '>Q', (2 << 32))]))],
        'tree_sibling': [(bno_at, amended(bno, 52, [(12, '>I', be32(agf, 16))]))],
        'dual_tree_disagreement': [(cnt_at, amended(cnt, 52, [(56, '>I', be32(cnt, 56) + 1)]))],
        'journal_checksum': [(last + 32, bytes([header[32] ^ 1]))],
        'journal_needs_replay': [(last + 512 + 9, b'\x00')],
        'journal_cycle_discontinuity': [(log_at + (head + 1)*512, struct.pack('>I', 7))],
    }
    png = files['sample.png']
    area = bounds({'png':png}, 'deleted_offset')
    for label, patches in edits.items():
        with changed(image, patches):
            result, rows = scan(binary, image, root/label, extensions='png', bounds=area)
            check([(r['record'], r['allocation_status']) for r in rows] ==
                  [(png['deleted_offset'], 'unknown')], (label, rows, result))
            check(any(w.startswith('ALLOCATION_FALLBACK:') for w in result['warnings']), label)
    # A consistent allocation change covers only the second block of a previously
    # found WAV. Its payload, mtime and all three fingerprint samples stay intact.
    wrapper = root/'reuse.raw'
    ((start, _),) = disk_image(wrapper, [image], sector=4096)
    shift = start * 4096
    stat = wrapper.stat()
    wav = files['sample.wav']
    area = bounds({'wav':wav}, 'deleted_offset', shift)
    before, (row,) = scan(binary, wrapper, root/'before-reuse', extensions='wav', bounds=area)
    check(row['record'] == shift + wav['deleted_offset'] and row['allocation_status'] == 'unallocated', row)
    block = wav['deleted_offset']//bs + 1
    check(wav['bytes'] > bs and all(be32(n, 56) + be32(n, 60) == block + 1 for n in [bno, cnt]), block)
    for at, original, pos, size, fmt, checksum in [
        (0, sb, 144, 8, '>Q', 224), (sector, agf, 52, 4, '>I', 216),
        (bno_at, bno, 60, 4, '>I', 52), (cnt_at, cnt, 60, 4, '>I', 52)
    ]:
        count = int.from_bytes(original[pos:pos+size], 'big')
        write_at(wrapper, shift+at, amended(original, checksum, [(pos, fmt, count - 1)]))
    os.utime(wrapper, ns=(stat.st_atime_ns, stat.st_mtime_ns))
    after, rows = scan(binary, wrapper, root/'after-reuse', extensions='wav', bounds=area)
    check(not rows, rows)
    check(not any('XFS allocation map' in w for w in after['warnings']), after)
    _, rows = scan(binary, wrapper, root/'reuse-all', scope='all', extensions='wav', bounds=area)
    check([r['record'] for r in rows] == [row['record']], rows)
    fingerprints = [json.loads(Path(v['session']).read_text(encoding='utf-8'))['fingerprint'] for v in [before, after]]
    check(fingerprints[0] == fingerprints[1], fingerprints)
    for mode in ([], ['--stream']):
        failure = run(binary, 'recover', *mode, '--session', before['session'], '--id', row['id'],
                      '--destination', root/('stale'+str(len(mode))), '--json', expected=5)
        check(failure['results'][0]['path'] is None and 'SOURCE_CHANGED' in json.dumps(failure), failure)
    wrapper.unlink()
    return sorted([*edits, 'partial_reuse', 'stale_recover', 'stale_stream'])


def verify(binary, root, xfs_tools=False):
    manifest = json.loads((REFERENCE/'manifest.json').read_text())
    manifest.update(json.loads((REFERENCE/'fragmented.json').read_text()))
    result = {}
    for case, ref in manifest.items():
        image = root/(case+'.raw')
        expand(REFERENCE/(case+'.raw.xz'), image, ref)
        if xfs_tools:
            independent(image, ref)
        files = {k: dict(v, deleted_bytes_retained=True) for k, v in ref['files'].items()}
        check(len(files) == 7, case)
        for field in ('deleted_offset', 'live_offset'):
            _, rows = scan(binary, image, root/(case+'-all-'+field), scope='all', bounds=bounds(files, field))
            check({r['record'] for r in rows} == {v[field] for v in files.values()}, rows)
        _, rows = scan(binary, image, root/(case+'-live'), bounds=bounds(files, 'live_offset'))
        check(not rows, (case, rows))
        area = bounds(files, 'deleted_offset')
        exact = restore(binary, image, files, root/(case+'-ordinary'), bounds=area)
        check(restore(binary, image, files, root/(case+'-indexed'), indexed=True, bounds=area) == exact, case)
        if case == 'xfs-4096-512-4':
            result['degraded'] = controls(binary, root, image, ref)
        check(file_hash(image) == ref['guest_sha256'], case)
        result[case] = dict(guest_sha256=ref['guest_sha256'], restored_sha256=exact, excluded_live=7,
                            xfs_db_extent_count=len(ref['xfs_db_free_extents']))
        print(case, '7 XFS unlinks byte- and SHA-256-exact, 7 allocated controls excluded', flush=True)
        image.unlink()
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--xfs-tools', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        report = verify(args.binary.resolve(), Path(tmp), args.xfs_tools)
    if args.output:
        args.output.write_text(json.dumps(report, indent=2, sort_keys=True)+'\n', encoding='utf-8')
