"""Recover HEIC and AVIF still images and compare native decoded images and full file contents."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / 'tests/heif'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def decoded(source):
    """Fingerprint PNG dimensions, sample format, palette, transparency and inflated scanlines."""
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(['heif-convert', source, Path(tmp)/'image.png'], check=True, capture_output=True)
        hashes = []
        for image in sorted(Path(tmp).glob('*.png')):
            png = image.read_bytes()
            check(png.startswith(b'\x89PNG\r\n\x1a\n'), image)
            pixels, header = bytearray(), bytearray()
            pos = 8
            while pos < len(png):
                n, kind = struct.unpack_from('>I4s', png, pos)
                data = png[pos+8:pos+8+n]
                if kind in (b'IHDR', b'PLTE', b'tRNS'):
                    header.extend(kind + data)
                if kind == b'IDAT':
                    pixels.extend(data)
                pos += 12+n
            check(pixels and header and pos == len(png), image)
            hashes.append(sha(header + zlib.decompress(pixels)))
        check(hashes, source)
        return hashes


def damaged(binary, root, files):
    heic, avif, tiled = (files[n] for n in ('odd.heic', 'color.avif', 'tiled.heic'))

    def change(data, kind, relative, replacement):
        at = data.index(kind) + 4 + relative
        return data[:at] + replacement + data[at+len(replacement):]

    cases = {
        'cut-hevc': heic[:-1], 'cut-av1': avif[:-1], 'cut-grid': tiled[:-7],
        'zero-mdat': change(heic,b'mdat',-8,bytes(4)),
        'handler': change(heic,b'hdlr',8,b'vide'),
        'unknown-primary': change(heic,b'pitm',4,b'\xff\xfe'),
        'external-data': change(heic,b'iloc',10,b'\0\x01'),
        'out-of-bounds': change(heic,b'iloc',12,b'\xff'*4),
        'protected': change(heic,b'infe',6,b'\0\x01'),
        'multiple-extents': change(heic,b'iloc',16,b'\0\x02'),
        'missing-configuration': change(heic,b'hvcC',-4,b'zzzz'),
        'hevc-array-type': change(heic,b'hvcC',23,b'\x3f'),
        'hevc-unit-length': change(heic,b'mdat',0,b'\xff'*4),
        'hevc-unit-header': change(heic,b'mdat',4,b'\xff'),
        'bad-av1-config': change(avif,b'av1C',0,b'\x01'),
        'av1-forbidden': change(avif,b'mdat',0,b'\xff'),
        'av1-obu-overflow': change(avif,b'mdat',0,b'\x0a'+b'\xff'*8),
        'grid-rows': change(tiled,b'idat',2,b'\x03'),
        'grid-dimension': change(tiled,b'idat',4,b'\0\x01'),
        'grid-cycle': change(tiled,b'dimg',4,b'\0\x01'),
        'sequence': change(heic,b'ftyp',8,b'msf1'),
        'mdat-unreferenced-tail': change(heic+b'!',b'mdat',-8,
            struct.pack('>I', len(heic) - (heic.index(b'mdat')-4) + 1)),
    }
    for name, data in cases.items():
        path = root / (name + '.raw')
        path.write_bytes(data)
        scan = run(binary,'scan','--source',path,'--mode','carve','--extension','heic','--extension','avif',
                   '--work-dir',root/name,'--json',expected=5)
        rows = run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(rows and all(not r['supported'] for r in rows), (name, rows))
    # An intact photo ends exactly before an unread block; another crosses a later unread block.
    disk = root/'gapped.raw'
    disk.write_bytes(b'!'*(4096-len(heic))+heic+bytes(8192-64)+avif+bytes(4096))
    result = run(binary, 'image', '--source', disk, '--destination', root/'acq', '--block-size', '4096', '--json')
    image = Path(result['image'])
    mark_gap(image,4096)
    mark_gap(image,12288)
    scan = run(binary,'scan','--source',image,'--mode','carve','--extension','heic','--extension','avif',
               '--work-dir',root/'gapped','--json',expected=5)
    rows = run(binary,'results','--session',scan['session'],'--json')['candidates']
    check({r['record_sha256'] for r in rows if r['supported']} == {sha(heic)}, rows)
    print(len(cases), 'damaged or unsupported photo cases and unread acquisition ranges checked', flush=True)
    return list(cases)


def recover(binary, root, disk, files, references, metadata):
    by_hash = {sha(data):name for name,data in files.items()}
    before = sha(disk.read_bytes())
    for name, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'heif',
                   '--extension', 'avif', '--work-dir', root/name, '--json', *flags, timeout=240)
        rows = run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(rows)==len(files) and all(r['supported'] and r['extent_evidence']=='media_structure' for r in rows)
              and {r['record_sha256'] for r in rows}==set(by_hash), rows)
        ids = [v for row in rows for v in ('--id', row['id'])]
        results = run(binary,'recover','--session',scan['session'],*ids,'--destination',root/(name+'-out'),'--json')['results']
        check(len(results)==len(files), results)
        for result in results:
            path = Path(result['path'])
            original = by_hash[result['sha256']]
            data = path.read_bytes()
            check(data==files[original] and sha(data)==result['sha256'], original)
            if original in metadata:
                check(all(data.count(b)==1 for b in metadata[original].values()), original)
            if references:
                check(decoded(path)==references[original], original)
                if original in metadata:
                    from heif_photo_metadata import read_metadata
                    check(read_metadata(path)==metadata[original], original)
        print(disk.name, name, len(results), 'still images match original bytes and SHA-256', flush=True)
    check(sha(disk.read_bytes())==before, disk)


def verify(binary, root, native=False):
    paths, meta = {}, {}
    for parent in (GOLD, GOLD/'metadata'):
        manifest = json.loads((parent/'manifest.json').read_text(encoding='utf-8'))
        check(not meta.keys() & manifest.keys(), 'duplicate HEIF fixture')
        meta.update(manifest)
        paths.update({name:parent/name for name in manifest})
    files = {name:path.read_bytes() for name,path in paths.items()}
    metadata = {name:{kind:base64.b64decode(data) for kind,data in item['jpeg_metadata'].items()}
                for name,item in meta.items() if 'jpeg_metadata' in item}
    hashes = {name:sha(data) for name,data in files.items()}
    check(all(meta[name]['bytes']==len(data) and meta[name]['sha256']==hashes[name] for name,data in files.items()), meta)
    if native:
        references = {name:decoded(path) for name,path in paths.items()}
        from heif_photo_metadata import read_metadata
        for name, blocks in metadata.items():
            check(read_metadata(paths[name])==blocks, name)
        check(all(len(frames)==(2 if name=='collection.heic' else 1) for name,frames in references.items()),references)
        for fs, create in [('fat32',make_deleted_fat), ('ext4',make_deleted_ext4)]:
            folder = root/fs
            folder.mkdir()
            disk = create(folder,files)
            recover(binary,folder,disk,files,references,metadata)
        return {'originals':hashes,'decoded':references,'metadata':{n:{k:sha(b) for k,b in m.items()} for n,m in metadata.items()},'filesystems':['fat32','ext4']}
    disk = root/'adjacent.raw'
    disk.write_bytes(b'!'*((1<<20)-3)+b''.join(files.values()))
    recover(binary,root,disk,files,None,metadata)
    return {'originals':hashes,'refused':damaged(binary,root,files)}


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--native',action='store_true')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        verify(args.binary.resolve(),Path(tmp),args.native)
