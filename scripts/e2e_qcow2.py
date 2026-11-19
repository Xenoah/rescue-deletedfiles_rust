"""Create standalone QCOW2 guest/host maps and verify the five restored files."""
import argparse
import hashlib
from pathlib import Path
import struct
import tempfile
import zlib

from e2e import check, run
from e2e_virtual_images import CORPUS

C = 8192
HEAD, REFS, COUNTS, L1, L2 = range(5)
COPIED = 1 << 63


def make(version, files):
    # A zero cluster separates the last partial guest block from the first
    # five. Payload clusters are allocated in reverse order. A separate
    # software encoder generates the compressed cluster and its sector count.
    logical = files[:]
    logical.insert(1, bytes(C))
    logical.append(bytes(512))
    contents = b''.join(b.ljust(C, b'\0') if i < len(logical)-1 else b for i, b in enumerate(logical))
    header, refs, counts, l1, l2 = (bytearray(C) for _ in range(5))
    struct.pack_into('>4sIQIIQIIQQIIQ', header, 0, b'QFI\xfb', version, 0, 0, 13, len(contents),
                     0, 1, L1*C, REFS*C, 1, 0, 0)
    if version == 3:
        struct.pack_into('>QQQII', header, 72, 0, 0, 0, 4, 104)
    struct.pack_into('>Q', refs, 0, COUNTS*C)
    struct.pack_into('>Q', l1, 0, COPIED | L2*C)
    host = bytearray()
    for i in reversed(range(len(logical))):
        block = logical[i].ljust(C, b'\0')
        if not any(block):
            entry = int(version == 3 and i == 1)  # Explicit all-zero cluster marker.
        else:
            at = 5*C + len(host)
            if i == 2:
                d = zlib.compressobj(wbits=-12)
                packed = d.compress(block) + d.flush()
                sectors = (len(packed) + 511)//512
                entry = (1 << 62) | ((sectors-1) << 57) | at
                host.extend(packed.ljust(C, b'\0'))
            else:
                entry = COPIED | at
                host.extend(block)
        struct.pack_into('>Q', l2, i*8, entry)
    for i in range(5 + len(host)//C):
        struct.pack_into('>H', counts, i*2, 1)
    return bytes(header + refs + counts + l1 + l2 + host), contents


def verify(binary, root):
    files = [(CORPUS / ('sample.' + e)).read_bytes() for e in ('avi','mkv','webm','ogg','gz')]
    hashes = {hashlib.sha256(b).hexdigest() for b in files}
    proof = {}
    for version in (2, 3):
        label = 'v' + str(version)
        image = root / (label + '.img')
        container, contents = make(version, files)
        image.write_bytes(container)
        output = root / (label + '.raw')
        result = run(binary, 'import-image', '--source', image, '--output', output, '--json')
        check(output.read_bytes() == contents and result['bytes'] == len(contents)
              and result['sha256'] == hashlib.sha256(contents).hexdigest(), result)
        scan = run(binary, 'scan', '--source', output, '--mode', 'carve', '--work-dir', root / label, '--json')
        candidates = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        restored = []
        for c in candidates:
            row, = run(binary, 'recover', '--session', scan['session'], '--id', c['id'],
                       '--destination', root / (label + '-files'), '--json')['results']
            data = Path(row['path']).read_bytes()
            check(data in files and hashlib.sha256(data).hexdigest() == row['sha256'], row)
            restored.append(row['sha256'])
        check(len(restored) == 5 and set(restored) == hashes and image.read_bytes() == container, label)
        proof[label] = {'guest_sha256': result['sha256'], 'restored_sha256': sorted(restored)}
        print(label, '5 restored files, RAW SHA-256', result['sha256'])
    def replace(at, be):
        return container[:at] + be + container[at + len(be):]
    for label, damaged in [
        ('backing', replace(8, struct.pack('>QI', 128, 12))),
        ('encryption', replace(32, struct.pack('>I', 1))),
        ('snapshot', replace(60, struct.pack('>I', 1))),
        ('dirty', replace(72, struct.pack('>Q', 1))),
        ('external-data', replace(72, struct.pack('>Q', 4))),
        ('unknown-incompatible', replace(72, struct.pack('>Q', 1 << 42))),
        ('l2-overlap', replace(L1*C, struct.pack('>Q', COPIED | L1*C))),
        ('payload-overlap', replace(L2*C, struct.pack('>Q', COPIED | COUNTS*C))),
        ('bad-l1', replace(L1*C, struct.pack('>Q', COPIED | (len(container) + C)))),
        ('bad-inflate', replace(8*C, b'\xff'*32)),
        ('truncation', container[:-1]),
    ]:
        image = root / (label + '.qcow2')
        failed = root / (label + '.raw')
        image.write_bytes(damaged)
        run(binary, 'import-image', '--source', image, '--image-type', 'qcow2',
            '--output', failed, '--json', expected=4)
        check(not failed.exists() and image.read_bytes() == damaged, label)
    return proof


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    args = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(), Path(tmp))
