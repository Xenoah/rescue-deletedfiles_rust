"""Restore files using only their recorded size/SHA-256 and an optional first offset."""
import argparse
import concurrent.futures
import json
import random
import re
import struct
import subprocess
import tempfile
from pathlib import Path

from e2e import check, run
from e2e_fingerprinted import samples, sha
from e2e_imaging import mark_gap
from e2e_raid import encode, save


def recover(binary, image, out, data, *flags, expected=0):
    return run(binary, 'recover-by-hash', '--source', image, '--output', out,
               '--file-bytes', len(data), '--sha256', sha(data), '--json', *flags, expected=expected)


def verify_result(report, out, original, image_bytes, spans=None):
    actual = [(s['offset'], s['length']) for s in report['extents']]
    check(report['original_sha256_verified'] and report['file']['bytes'] == len(original), report)
    check(out.read_bytes() == original and sha(out.read_bytes()) == report['file']['sha256'] == sha(original), out)
    check(b''.join(image_bytes[p:p+n] for p, n in actual) == original, actual)
    if spans is not None:
        check(actual == spans, (actual, spans))


def portable(binary, root):
    files = samples()
    rng = random.Random(5521)
    image = root/'media.raw'
    # Odd origin, non-sector-aligned EOF and both physical fragment orders.
    for i, (name, data) in enumerate(files.items()):
        split = 512 * (1 + i % 3)
        head, tail = (4103, 17415) if i % 2 else (17415, 7)
        raw = bytearray(rng.randbytes(24655))
        raw[head:head+split] = data[:split]
        raw[tail:tail+len(data)-split] = data[split:]
        image.write_bytes(raw)
        out = root/name
        report = recover(binary, image, out, data, '--start', 7, '--first-offset', head)
        verify_result(report, out, data, raw)
        check(len(report['extents']) == 2, name)
        out.unlink()
        recover(binary, image, out, data, '--start', 7, expected=4)
        check(not out.exists(), name)
        out_at = len(raw)-len(data)
        raw[out_at:] = data
        image.write_bytes(raw)
        report = recover(binary, image, out, data, '--start', out_at % 512)
        verify_result(report, out, data, raw, [(out_at, len(data))])

    data = files['VENDOR.BIN']
    raw = bytearray(rng.randbytes(32768))
    raw[2051:2051+len(data)] = data
    image.write_bytes(raw)
    out = root/'refused'
    # Explicit anchor need not be on the grid for unknown locations.
    report = recover(binary, image, out, data, '--first-offset', 2051)
    verify_result(report, out, data, raw, [(2051,len(data))])
    out.unlink()
    for flags, code, reason in [
            (['--first-offset',2052],4,'no whole-file SHA-256 match'),
            (['--end',3000],4,'shorter'),
            (['--start',2052,'--first-offset',2051],2,'invalid search range'),
            (['--block-size',3],2,'power of two'),
            (['--max-work',1],4,'byte budget'),
            (['--max-work',0],2,'search byte budget')]:
        msg = recover(binary, image, out, data, *flags, expected=code)
        check(reason in str(msg) and not out.exists(), msg)
    wrong = bytearray(data)
    wrong[-1] ^= 1
    msg = recover(binary,image,out,wrong,'--first-offset',2051,expected=4)
    check('no whole-file SHA-256 match' in str(msg) and not out.exists(),msg)
    for size, digest in [(0,sha(b'')),(64*1024*1024+1,sha(data)),(512,'x'*64)]:
        run(binary,'recover-by-hash','--source',image,'--output',out,'--file-bytes',size,
            '--sha256',digest,'--json',expected=2)
        check(not out.exists(),'bad arguments')
    # Physical overlap alone would reconstruct this digest if overlap were admitted.
    overlap = bytes(raw[2048:3072]+raw[2560:3072])
    recover(binary,image,out,overlap,'--first-offset',2048,expected=4)
    check(not out.exists(),'overlap')
    cfg,_ = encode(root/'members',bytes(raw),'raid0',n=2)
    report = recover(binary,save(root,cfg),out,data,'--first-offset',2051)
    verify_result(report,out,data,raw)
    out.unlink()
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        def attempt(_):
            args = [binary,'recover-by-hash','--source',image,'--output',out,'--file-bytes',str(len(data)),
                    '--sha256',sha(data),'--first-offset','2051']
            return subprocess.run(args,capture_output=True).returncode
        codes = list(pool.map(attempt,range(3)))
    check(codes.count(0) == 1 and out.read_bytes() == data,codes)
    out.unlink()
    recover(binary,image,Path(str(image)+'.rmap'),data,'--first-offset',2051,expected=4)
    check(not Path(str(image)+'.rmap').exists(),'read-map path alias')
    zero = root/'zero.raw'
    zero.write_bytes(bytes(8192))
    acquired = Path(run(binary,'image','--source',zero,'--destination',root/'acquired',
                        '--block-size',512,'--json')['image'])
    gap, length = mark_gap(acquired,1024)
    error = recover(binary,acquired,out,bytes(512),'--start',gap,'--end',gap+length,expected=4)
    check('reads intersected missing ranges' in str(error) and not out.exists(),error)
    report = recover(binary,acquired,out,bytes(512),'--start',gap)
    verify_result(report,out,bytes(512),zero.read_bytes(),[(gap+length,512)])
    check(report['reads_with_gaps'] > 0,report)
    check(image.read_bytes()==raw,'source changed')
    print('Seven byte-exact contiguous and seven anchored two-run files; reversed runs, EOF, odd grid, '
          'RAID0, missing sectors, concurrent publication, bad hashes and work limits checked',flush=True)
    return {name:sha(data) for name,data in files.items()}


def mtool(*args):
    return subprocess.run(list(map(str,args)),check=True,capture_output=True,timeout=120).stdout


def geometry(raw):
    bps, spc, reserved, fats, root_entries = struct.unpack_from('<HBHBH',raw,11)
    fat_size = struct.unpack_from('<H',raw,22)[0] or struct.unpack_from('<I',raw,36)[0]
    sectors = reserved + fats*fat_size + (root_entries*32+bps-1)//bps
    return sectors*bps, spc*bps


def native(binary, root):
    files = samples()
    evidence = {}
    for kind, disk_bytes, flags in [(12,1440*1024,['-f','1440']), (16,32<<20,['-c','2']),
                                    (32,40<<20,['-F','-c','1'])]:
        folder = root/f'fat{kind}'
        folder.mkdir()
        image = folder/'source.raw'
        with image.open('wb') as f:
            f.truncate(disk_bytes)
        mtool('mformat','-i',image,*flags,'::')
        with image.open('rb') as f:
            boot = f.read(512)
        base, cluster = geometry(boot)
        count = (disk_bytes-base)//cluster
        check((12 if count<4085 else 16 if count<65525 else 32)==kind,(kind,count,cluster))
        starts = {}
        for i, (name, data) in enumerate(files.items()):
            payload = folder/name
            payload.write_bytes(data)
            count = (len(data)+cluster-1)//cluster
            split = max(1, count//2)
            for label,n in [('A',split),('G',1),('B',count-split),('K',1)]:
                path = folder/f'{label}{i}.TMP'
                path.write_bytes(bytes([90+i])*cluster*n)
                mtool('mcopy','-i',image,path,'::')
        free_line = next(l for l in mtool('mdir','-i',image,'::').decode().splitlines() if 'bytes free' in l)
        free = int(re.sub(r'\D','',free_line))
        tail = folder/'FILL.BIN'
        with tail.open('wb') as f:
            f.truncate(free)
        mtool('mcopy','-i',image,tail,'::')
        for i, (name,data) in enumerate(files.items()):
            mtool('mdel','-i',image,f'::A{i}.TMP',f'::B{i}.TMP')
            mtool('mcopy','-i',image,folder/name,'::')
            allocation = mtool('mshowfat','-i',image,'::'+name).decode()
            chain = re.findall(r'<(\d+)(?:-(\d+))?>',allocation)
            check(len(chain)==2,allocation)
            spans = []
            left = len(data)
            for a,b in chain:
                n = min(left,(int(b or a)-int(a)+1)*cluster)
                spans.append((base+(int(a)-2)*cluster,n))
                left -= n
            check(left==0 and mtool('mtype','-i',image,'::'+name)==data,name)
            starts[name]=spans
        mtool('mdel','-i',image,*['::'+name for name in files])
        listing = mtool('mdir','-b','-i',image,'::').decode().upper()
        check(all(name not in listing for name in files),'directory entry survived unlink')
        raw = image.read_bytes()
        digests = {}
        for name,data in files.items():
            spans = starts[name]
            out = folder/('restored-'+name)
            # Use only geometry, the pre-deletion first offset, size and whole SHA.
            # The complete independently recorded mshowfat chain is the test oracle.
            report = recover(binary,image,out,data,'--block-size',cluster,'--first-offset',spans[0][0],
                             '--start',base,'--end',base+128*cluster)
            verify_result(report,out,data,raw,spans)
            digests[name] = {'sha256':sha(data),'bytes':len(data),'extents':report['extents']}
        check(sha(image.read_bytes())==sha(raw),'disk changed')
        evidence[str(kind)] = digests
        print(f'FAT{kind}, {cluster}-byte clusters: seven actual two-run unlinks recovered, '
              'original bytes, SHA-256 and independent mtools extents matched',flush=True)
    return evidence


if __name__=='__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',required=True,type=Path)
    parser.add_argument('--native',action='store_true')
    parser.add_argument('--evidence',type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as d:
        result = (native if args.native else portable)(args.binary.resolve(),Path(d))
    if args.evidence:
        args.evidence.write_text(json.dumps(result,indent=2)+'\n')
