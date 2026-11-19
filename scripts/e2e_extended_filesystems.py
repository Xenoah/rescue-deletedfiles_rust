"""Recover actual filesystem deletions on disposable ext2/3/4 images and independent containers."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests' / 'corpus' / 'files'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def command(*args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, capture_output=True, **kwargs)


def make_deleted(root, filesystem, samples):
    raw = root / (filesystem + '.img')
    with raw.open('wb') as image:
        image.truncate(16 * 1024 * 1024)
    command('mke2fs', '-q', '-t', filesystem, '-F', raw)
    # Allocate all originals before unlinking any of them, so no inode gets reused.
    commands = [f'write {name} /{name}' for name in samples] + [
        f'rm /{name}' for name in samples]
    command('debugfs', '-w', '-f', '-', raw, input='\n'.join(commands) + '\n',
            text=True, cwd=root)
    return raw


def verify(binary, root, native):
    samples = {f'sample.{ext}': (CORPUS / f'sample.{ext}').read_bytes()
               for ext in ['gif', 'bmp', 'webp', 'wav', 'sqlite', 'txt']}
    samples['vendor_format'] = bytes(range(256)) * 37
    for name, data in samples.items():
        (root / name).write_bytes(data)

    def recover(label, sources, extra=()):
        before = [sha(p.read_bytes()) for p in sources]
        args = ['--source', sources[0], *extra]
        for segment in sources[1:]:
            args.extend(['--segment', segment])
        listing = run(binary, 'filesystem', *args, '--json')['candidates']
        check({row['original_path'] for row in listing} == samples.keys(), listing)
        dest = root / (label + '-out')
        rows = run(binary, 'filesystem', *args, '--all', '--destination', dest, '--json')['results']
        check(len(rows) == len(samples), rows)
        for row in rows:
            original = samples[row['original_path']]
            check(row['sha256'] == sha(original) and Path(row['path']).read_bytes() == original, row)
        first = rows[0]
        collision = run(binary, 'filesystem', *args, '--id', first['id'], '--destination', dest,
                        '--json', expected=5)['results'][0]
        check('error' in collision and sha(Path(first['path']).read_bytes()) == first['sha256'], collision)
        check([sha(p.read_bytes()) for p in sources] == before, label + ' source changed')
        print(label, len(rows), 'deleted files byte-exact; source unchanged', flush=True)

    for fs in ['ext2', 'ext3', 'ext4']:
        raw = make_deleted(root, fs, samples)
        recover(fs, [raw], ['--fs-type', fs])
    if native:
        from e2e_disks import native_source
        before = sha(raw.read_bytes())
        dest = root / 'native-out'
        dest.mkdir()
        with native_source(raw, root) as (prefix, device, _):
            rows = json.loads(command(*prefix, binary, 'filesystem', '--source', device, '--all',
                                      '--destination', dest, '--json').stdout)['results']
            check(len(rows) == len(samples), rows)
            for row in rows:
                data = command(*prefix, 'cat', row['path']).stdout
                check(data == samples[row['original_path']] and sha(data) == row['sha256'], row)
        check(sha(raw.read_bytes()) == before, 'loop device source changed')
        print('read-only Linux loop device', len(rows), 'deleted files byte-exact', flush=True)
    expected = raw.read_bytes()
    for fmt, suffix in [('vpc', 'vhd'), ('vmdk', 'vmdk')]:
        image = root / ('disk.' + suffix)
        options = ['-o', 'force_size=on'] if fmt == 'vpc' else []
        command('qemu-img', 'convert', '-f', 'raw', '-O', fmt, *options, raw, image)
        before = sha(image.read_bytes())
        recover(suffix, [image], ['--image-type', suffix])
        imported = root / (suffix + '-decoded.raw')
        result = run(binary, 'import-image', '--source', image, '--image-type', suffix,
                     '--output', imported, '--json')
        check(result['sha256'] == sha(expected) and imported.read_bytes() == expected, suffix)
        check(sha(image.read_bytes()) == before, suffix + ' import modified source')
        print(suffix, 'logical RAW image byte-exact', flush=True)
    # Compare the built-in VDI reader against qemu-img, then recover the real
    # debugfs deletions from its output using the same metadata workflow.
    for fmt, options in [('dynamic-vdi', []), ('fixed-vdi', ['-o', 'static=on'])]:
        image = root / (fmt + '.image')
        command('qemu-img', 'convert', '-f', 'raw', '-O', 'vdi', *options, raw, image)
        before = sha(image.read_bytes())
        imported = root / (fmt + '-decoded.raw')
        flags = ['--image-type', 'vdi'] if options else []
        result = run(binary, 'import-image', '--source', image, '--output', imported, '--json', *flags)
        check(result['bytes'] == len(expected) and result['sha256'] == sha(expected)
              and imported.read_bytes() == expected and sha(image.read_bytes()) == before, fmt)
        recover(fmt, [imported], ['--fs-type', 'ext4'])
    for label, options in [('qcow2-v2', ['-o', 'compat=0.10']),
                           ('qcow2-v3', []), ('qcow2-deflate', ['-c']),
                           ('qcow2-4k', ['-o', 'cluster_size=4096']),
                           ('qcow2-2m', ['-o', 'cluster_size=2M'])]:
        image = root / (label + '.img')
        command('qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', *options, raw, image)
        before = sha(image.read_bytes())
        imported = root / (label + '.raw')
        flags = [] if options else ['--image-type', 'qcow2']
        result = run(binary, 'import-image', '--source', image, '--output', imported, '--json', *flags)
        check(result['bytes'] == len(expected) and result['sha256'] == sha(expected)
              and imported.read_bytes() == expected and sha(image.read_bytes()) == before, label)
        recover(label, [imported], ['--fs-type', 'ext4'])
    overlay = root / 'overlay.qcow2'
    command('qemu-img', 'create', '-f', 'qcow2', '-F', 'raw', '-b', raw, overlay)
    rejected = root / 'overlay.raw'
    run(binary, 'import-image', '--source', overlay, '--output', rejected, '--json', expected=4)
    check(not rejected.exists() and sha(raw.read_bytes()) == sha(expected), 'external backing file')
    parts = [root / 'split.first', root / 'split.second']
    parts[0].write_bytes(expected[:len(expected)//2])
    parts[1].write_bytes(expected[len(expected)//2:])
    recover('split-raw', parts, ['--image-type', 'raw'])
    whole = root / 'partitioned.img'
    whole.write_bytes(bytes(2048*512) + expected)
    recover('volume-offset', [whole], ['--sector-offset', '2048'])
    # A failed reader may already have written to stdout. No final output may be published.
    output = root / 'bad-decoding.raw'
    run(binary, 'import-image', '--source', raw, '--image-type', 'vmdk', '--output', output,
        '--json', expected=4)
    check(not output.exists(), 'failed import left an output')
    run(binary, 'import-image', '--source', raw, '--output', raw, '--json', expected=2)
    check(raw.read_bytes() == expected, 'input overwritten')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--native', action='store_true', help='Linux CI: attach an owned image on a read-only loop device')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp), args.native)
