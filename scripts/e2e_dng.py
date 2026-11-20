"""Recover Bayer DNG originals across strips, tiles, packed samples and lossless JPEG."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_jpeg2000 import candidates
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1] / 'tests/dng'
FILTER = ['--extension', 'dng']


def sha(data):
    return hashlib.sha256(data).hexdigest()


def uncompressed_tiles(path, meta):
    data = path.read_bytes()
    order, _, fields, _ = tiff_rows(data)

    def unsigned(tag):
        row = fields[tag]
        ty, count = struct.unpack_from(order+'HI', data, row+2)
        width, letter = {3:(2,'H'), 4:(4,'I')}[ty]
        at = row+8 if width*count <= 4 else struct.unpack_from(order+'I', data, row+8)[0]
        return struct.unpack_from(order + str(count) + letter, data, at)

    w, h, bits = meta['width'], meta['height'], meta['bits']
    tw, = unsigned(322)
    th, = unsigned(323)
    offsets, counts = unsigned(324), unsigned(325)
    across = (w + tw - 1) // tw
    size = bits // 8
    result = bytearray(w*h*2)
    check(bits in (8,16) and len(offsets)==len(counts)==across*((h+th-1)//th), path)
    for i,(at,count) in enumerate(zip(offsets,counts)):
        check(count==tw*th*size and at+count<=len(data), path)
        left,top=(i%across)*tw,(i//across)*th
        endian = 'little' if order=='<' else 'big'
        for y in range(min(th,h-top)):
            for x in range(min(tw,w-left)):
                start=at+(y*tw+x)*size
                value=int.from_bytes(data[start:start+size],endian)
                struct.pack_into('<H',result,2*((top+y)*w+left+x),value)
    check(sha(result)==meta['samples_u16le_sha256'],path)
    return sha(result)


def sample_digest(path, meta):
    # LibRaw 0.20.2 misassembles uncompressed DNG tiles. Check their full raster
    # independently, including partial edge tiles and the file's TIFF byte order.
    if meta['encoding']=='raw' and meta['tiled']:
        return uncompressed_tiles(path, meta)
    with tempfile.TemporaryDirectory() as folder:
        # LibRaw writes beside its input. Keep the checked originals read-only.
        sample = Path(folder) / 'sensor.dng'
        sample.write_bytes(path.read_bytes())
        subprocess.run(['unprocessed_raw', '-q', sample], capture_output=True, check=True, timeout=60)
        pgm, = Path(folder).glob('*.pgm')
        magic, dimensions, maximum, values = pgm.read_bytes().split(b'\n', 3)
    check((magic, maximum) == (b'P5', b'65535'), path)
    check(tuple(map(int, dimensions.split())) == (meta['width'], meta['height']), dimensions)
    check(len(values) == meta['width'] * meta['height'] * 2, path)
    little = bytearray(values)
    little[::2], little[1::2] = little[1::2], little[::2]
    check(sha(little) == meta['samples_u16le_sha256'], path)
    return sha(little)


def recover(binary, disk, folder, files, manifest, native):
    hashes = {sha(data):name for name,data in files.items()}
    for label, flags in [('ordinary', []), ('indexed', ['--indexed'])]:
        session, rows = candidates(binary, disk, folder / label, FILTER + flags)
        check(len(rows) == len(files) and {r['record_sha256'] for r in rows} == hashes.keys(), rows)
        check(all(r['supported'] and r['extent_evidence'] == 'file_structure' and
                  r['name'].endswith('.dng') for r in rows), rows)
        saved = run(binary, 'recover', '--session', session, '--all', '--destination',
                    folder / (label + '-out'), '--json', timeout=240)['results']
        check(len(saved) == len(files), saved)
        for row in saved:
            name = hashes[row['sha256']]
            path = Path(row['path'])
            check(row['status'] == 'completed' and path.read_bytes() == files[name] and
                  sha(path.read_bytes()) == manifest[name]['sha256'], row)
            if native:
                sample_digest(path, manifest[name])
    return {name:sha(data) for name,data in files.items()}


def tiff_rows(data):
    endian = {b'II':'<', b'MM':'>'}[data[:2]]
    root, = struct.unpack_from(endian + 'I', data, 4)
    count, = struct.unpack_from(endian + 'H', data, root)
    pairs = {}
    for i in range(count):
        row = root + 2 + 12*i
        tag, = struct.unpack_from(endian + 'H', data, row)
        pairs[tag] = row
    return endian, root, pairs, root + 2 + 12*count


def controls(binary, folder, files):
    raw = files['raw-16-1.dng']
    endian, root, tags, next_ptr = tiff_rows(raw)
    u16 = lambda n:struct.pack(endian+'H', n)
    u32 = lambda n:struct.pack(endian+'I', n)

    def alter(data, pos, replacement):
        return data[:pos] + replacement + data[pos + len(replacement):]

    def value(tag, replacement):
        return alter(raw, tags[tag]+8, replacement)

    offset_table, = struct.unpack_from(endian+'I', raw, tags[273]+8)
    lengths, = struct.unpack_from(endian+'I', raw, tags[279]+8)
    first_pixel, second_pixel = struct.unpack_from(endian+'II', raw, offset_table)
    cases = {
        'unknown-version':value(50706, bytes((2,0,0,0))), 'backward-version-newer':value(50707, bytes((1,5,0,0))),
        'version-wrong-type':alter(raw,tags[50706]+2,u16(3)), 'subdirectory-cycle':alter(raw,next_ptr,u32(root)),
        'subdirectory-outside':alter(raw,next_ptr,u32(len(raw)+4096)), 'duplicate-field':alter(raw,tags[305],u16(256)),
        'unhandled-pointer':alter(raw,tags[305],u16(50933)), 'invalid-type':alter(raw,tags[305]+2,u16(17)),
        'value-offset-outside':value(305,u32(len(raw)+300)), 'huge-count':alter(raw,tags[305]+4,u32(0xffffffff)),
        'unknown-compression':value(259,u16(52546)), 'lossy-dng':value(259,u16(34892)), 'bad-depth':value(258,u16(33)),
        'non-bayer':value(33422,bytes((1,1,2,0))), 'wrong-cfa-raster':value(33421,u16(1)+u16(4)),
        'multiple-samples':value(277,u16(3)), 'zero-width':value(256,u32(0)), 'excessive-height':value(257,u32(0xffffffff)),
        'aliased-strips':alter(raw,offset_table+4,u32(first_pixel)), 'overlapping-strips':alter(raw,offset_table+4,u32(first_pixel+1)),
        'pixel-over-directory':alter(raw,offset_table,u32(root)), 'truncated-raster':raw[:-1],
        'encoded-length-mismatch':alter(raw,lengths,u16(1)), 'pixel-extent-outside':alter(raw,offset_table+4,u32(len(raw)+200)),
    }
    check(second_pixel > first_pixel, 'strip offsets')
    coded = files['turbo-16-14.dng']
    end, _, fields, _ = tiff_rows(coded)
    at, = struct.unpack_from(end+'I', coded, fields[273]+8)
    frame = coded.index(b'\xff\xc3',at)
    scan = coded.index(b'\xff\xda',frame)
    entropy = scan + 2 + int.from_bytes(coded[scan+2:scan+4],'big')
    cases.update({
        'wrong-jpeg-precision':alter(coded,frame+4,b'\x07'),
        'wrong-jpeg-geometry':alter(coded,frame+5,b'\0\1'),
        'unknown-predictor':alter(coded,scan+7,b'\0'),
        'unexpected-scan-end':alter(coded,entropy+5,b'\xff\xd9'),
        'missing-final-eoi':coded[:-2]+bytes(2),
        'arithmetic-frame':alter(coded,frame+1,b'\xcb'),
        'unlisted-dc-table':alter(coded,scan+6,b'\x30'),
    })
    for label, data in cases.items():
        disk = folder / (label+'.raw')
        disk.write_bytes(data)
        _, rows = candidates(binary, disk, folder / (label+'-work'), FILTER, expected=5)
        check(rows and not any(r['supported'] for r in rows), (label,rows))
    padded = ((len(raw)+4095)//4096)*4096
    for label, gap in [('gap-inside',4096),('gap-after',padded)]:
        disk = folder / (label+'.raw')
        disk.write_bytes(bytes(padded-len(raw))+raw+bytes(8192))
        captured = run(binary,'image','--source',disk,'--block-size','4096','--destination',folder/(label+'-copy'),'--json')
        image = Path(captured['image'])
        mark_gap(image,gap)
        session, rows = candidates(binary,image,folder/(label+'-scan'),FILTER,expected=5)
        if label == 'gap-inside':
            check(not any(r['supported'] for r in rows),rows)
        else:
            one, = [r for r in rows if r['supported']]
            saved = run(binary,'recover','--session',session,'--id',one['id'],'--destination',folder/'gap-out','--json')['results'][0]
            check(Path(saved['path']).read_bytes()==raw and saved['sha256']==sha(raw),saved)
    changed=alter(raw,first_pixel+1,bytes([raw[first_pixel+1]^1]))
    modified=folder/'changed.raw'
    modified.write_bytes(changed)
    session,rows=candidates(binary,modified,folder/'changed-scan',FILTER)
    check(len(rows)==1 and rows[0]['record_sha256']==sha(changed) and sha(changed)!=sha(raw),rows)
    report=run(binary,'recover','--session',session,'--all','--destination',folder/'changed-out','--json')
    known=folder/'expected.sha256'
    known.write_text(sha(raw)+'  original.dng\n')
    compared=run(binary,'match-hashes','--report',report['report'],'--checksums',known,'--json')['comparison']
    check(compared['counts']['not_listed']==1,compared)
    return {'refused':sorted([*cases,'gap-inside']),'adjacent_gap':sha(raw),'changed_sensor_sample':'not_listed'}


def verify(binary, folder, native):
    manifest = json.loads((GOLD/'manifest.json').read_text())
    files = {name:(GOLD/name).read_bytes() for name in manifest}
    for name,data in files.items():
        check(len(data)==manifest[name]['bytes'] and sha(data)==manifest[name]['sha256'],name)
        if native:
            sample_digest(GOLD/name,manifest[name])
    result = {}
    for label in (('fat32','ext4') if native else ('adjacent',)):
        where=folder/label
        where.mkdir()
        if label=='fat32':
            disk=make_deleted_fat(where,files)
        elif label=='ext4':
            disk=make_deleted_ext4(where,files,block_size=4096)
        else:
            disk=where/'samples.raw'
            disk.write_bytes(b'!'*((1<<20)-2)+b''.join(files.values())+b'nonzero-unreferenced-trailer')
        before=sha(disk.read_bytes())
        result[label]=recover(binary,disk,where,files,manifest,native)
        check(sha(disk.read_bytes())==before,disk)
        print(label,len(files),'DNG originals byte- and SHA-256-exact in both scan modes',flush=True)
    result.update(controls(binary,folder,files))
    if native:
        result['sensor_arrays']={name:item['samples_u16le_sha256'] for name,item in manifest.items()}
    return result


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--native',action='store_true')
    parser.add_argument('--output',type=Path)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result=verify(args.binary.resolve(),Path(tmp),args.native)
    if args.output:
        args.output.write_text(json.dumps(result,sort_keys=True,indent=2)+'\n')
