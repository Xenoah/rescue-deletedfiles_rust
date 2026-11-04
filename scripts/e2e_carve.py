"""PNG carving verification; byte-exact CLI/MCP outputs and malformed structures."""
import argparse
import hashlib
import json
import os
import struct
import tempfile
import zlib
from pathlib import Path
from e2e import Client, check, run
from make_carve_fixture import SIGNATURE, build, chunk, png


def scan(binary, image, work, expected=0):
    return run(binary, 'scan', '--mode', 'carve', '--source', image, '--work-dir', work, '--json', expected=expected)


def main(binary):
    cases = 0
    with tempfile.TemporaryDirectory(prefix='rescue-carve-') as temporary:
        root = Path(temporary)
        source = root/'source.raw'
        truth = build(source)
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        summary = scan(binary, source, root/'work', expected=5)
        check(summary['filesystem'] == 'raw' and summary['progress_unit'] == 'bytes', summary)
        check(summary['scanned_records'] == source.stat().st_size and summary['invalid_records'] == 1, summary)
        candidates = run(binary, 'results', '--session', summary['session'], '--json')['candidates']
        check({c['id'] for c in candidates if c['supported']} == set(truth), candidates)
        check(all(c['allocation_status'] == 'unknown' and not c['path_verified'] for c in candidates), candidates)
        recovered = run(binary, 'recover', '--session', summary['session'], '--all', '--destination', root/'out', '--json')
        for result in recovered['results']:
            expected = truth[result['candidate_id']]
            check(result['status'] == 'completed' and Path(result['path']).read_bytes() == expected, result)
            check(result['sha256'] == hashlib.sha256(expected).hexdigest(), result)
            check(result['extent_evidence'] == 'png_validated' and result['allocation_status'] == 'unknown', result)
        check(hashlib.sha256(source.read_bytes()).hexdigest() == before, 'source modified')
        cases += 1
        # Every legal static color/depth combination, all filters, and Adam7.
        variants = [png(color=c, depth=d, interlace=True, split=True) for c, ds in
                    [(0, [1,2,4,8,16]), (2,[8,16]), (3,[1,2,4,8]), (4,[8,16]), (6,[8,16])] for d in ds]
        variants += [png(filter_type=f) for f in range(5)]
        image = root/'variants.raw'; image.write_bytes(b'\x00'.join(variants))
        good = scan(binary, image, root/'variants-work')
        check(good['candidates'] == len(variants), good)
        outputs = run(binary, 'recover', '--session', good['session'], '--all', '--destination', root/'variants-out', '--json')['results']
        check([Path(r['path']).read_bytes() for r in outputs] == variants, 'variant byte mismatch')
        cases += 1

        header = chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
        compressed = zlib.compress(b'\0\1\2\3')
        end = chunk(b'IEND', b'')
        bad_adler = compressed[:-1] + bytes([compressed[-1]^1])
        invalid = {
            'missing_adler': SIGNATURE+header+chunk(b'IDAT',compressed[:-4])+end,
            'cut_deflate_end': SIGNATURE+header+chunk(b'IDAT',compressed[:-5])+end,
            'extra_pixels': SIGNATURE+header+chunk(b'IDAT',zlib.compress(b'\0\1\2\3'*2))+end,
            'extra_zlib': SIGNATURE+header+chunk(b'IDAT',compressed+zlib.compress(b'junk'))+end,
            'palette_index': SIGNATURE+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,3,0,0,0))+chunk(b'PLTE',bytes(3))+chunk(b'IDAT',zlib.compress(b'\0\xff'))+end,
            'palette_depth': SIGNATURE+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,1,3,0,0,0))+chunk(b'PLTE',bytes(9))+chunk(b'IDAT',zlib.compress(b'\0\0'))+end,
            'gray_palette': SIGNATURE+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,0,0,0,0))+chunk(b'PLTE',bytes(3))+chunk(b'IDAT',zlib.compress(b'\0\xff'))+end,
            'truncated': png()[:-3],
            'crc': png()[:-1] + bytes([png()[-1]^1]),
            'zlib': SIGNATURE+header+chunk(b'IDAT', b'broken')+end,
            'adler': SIGNATURE+header+chunk(b'IDAT', bad_adler)+end,
            'short_pixels': SIGNATURE+header+chunk(b'IDAT', zlib.compress(b'\0\1'))+end,
            'filter': SIGNATURE+header+chunk(b'IDAT', zlib.compress(b'\5\1\2\3'))+end,
            'idat_gap': SIGNATURE+header+chunk(b'IDAT', compressed[:2])+chunk(b'tEXt',b'a\0b')+chunk(b'IDAT',compressed[2:])+end,
            'no_idat': SIGNATURE+header+end,
            'apng': SIGNATURE+header+chunk(b'acTL',struct.pack('>II',1,0))+chunk(b'IDAT',compressed)+end,
            'critical': SIGNATURE+header+chunk(b'ABCD',b'')+chunk(b'IDAT',compressed)+end,
            'huge_extent': SIGNATURE+header+struct.pack('>I', 0xffffffff)+b'IDAT',
            'huge_pixels': SIGNATURE+chunk(b'IHDR',struct.pack('>IIBBBBB',100000,100000,16,6,0,0,0))+chunk(b'IDAT',compressed)+end,
            'duplicate_header': SIGNATURE+header+header+chunk(b'IDAT',compressed)+end,
        }
        for label, data in invalid.items():
            image = root/(label+'.raw'); image.write_bytes(data)
            bad = scan(binary, image, root/(label+'-work'), expected=5)
            check(bad['candidates'] == 1 and bad['invalid_records'] == 1, (label,bad))
            candidate = run(binary, 'results','--session',bad['session'],'--json')['candidates'][0]
            check(not candidate['supported'], label)
            result = run(binary, 'recover','--session',bad['session'],'--id',candidate['id'],'--destination',root/(label+'-out'),'--json', expected=5)['results'][0]
            check(result['path'] is None, label)
            cases += 1
        # Serialized sizes/names cannot choose the output extent/name; forged offsets/hashes fail.
        original = json.loads(Path(summary['session']).read_text(encoding='utf-8'))
        first = next(c for c in original['candidates'] if c['supported'])
        for key, value in [('record', 2**64-1), ('record_sha256','0'*64), ('size',2**64-1), ('name','../escape.txt')]:
            edited = json.loads(json.dumps(original))
            candidate = next(c for c in edited['candidates'] if c['id'] == first['id'])
            candidate[key] = value
            path = root/(key+'.json'); path.write_text(json.dumps(edited), encoding='utf-8')
            rejects = key in ('record','record_sha256')
            result = run(binary, 'recover','--session',path,'--id',first['id'],'--destination',root/(key+'-out'),'--json', expected=5 if rejects else 0)['results'][0]
            if rejects:
                check(result['path'] is None, key)
            else:
                check(Path(result['path']).read_bytes() == truth[first['id']] and Path(result['path']).name.endswith(first['name']), key)
            cases += 1
        # Identical data at separate offsets remain distinct candidates.
        image = root/'duplicates.raw'; image.write_bytes(png()+png())
        duplicate = scan(binary, image, root/'duplicates-work')
        check(duplicate['candidates'] == 2, duplicate); cases += 1
        image = root/'empty.raw'; image.write_bytes(b'')
        check(scan(binary, image, root/'empty-work')['candidates'] == 0, 'empty image'); cases += 1

        # A changed PNG outside fingerprint samples, with the timestamp restored,
        # must still fail the full PNG hash check at recovery.
        saved = source.read_bytes(); stat = source.stat()
        at = 1024*1024-3
        changed = bytearray(saved)
        size_png = len(truth[f'png-{at:016x}'])
        part = changed[at:at+size_png]
        text = part.index(b'tEXt')
        size = struct.unpack('>I', part[text-4:text])[0]
        part[text+4] ^= 1
        part[text+4+size:text+8+size] = struct.pack('>I',zlib.crc32(part[text:text+4+size]))
        changed[at:at+size_png] = part
        source.write_bytes(changed)
        os.utime(source, ns=(stat.st_atime_ns,stat.st_mtime_ns))
        result = run(binary,'recover','--session',summary['session'],'--id',f'png-{at:016x}',
                     '--destination',root/'changed-out','--json',expected=5)['results'][0]
        check(result['path'] is None and 'SOURCE_CHANGED' in result['error'], result)
        source.write_bytes(saved)
        cases += 1

        client = Client(binary, source, root/'mcp-work', root/'mcp-out')
        try:
            client.rpc('initialize', {'protocolVersion':'2025-11-25'})
            for mode in ['unknown', 3, None]:
                client.tool('start_scan', {'source':str(source),'mode':mode,'request_id':'bad'}, error=True)
            args = {'source':str(source),'mode':'carve','request_id':'carve'}
            started = client.tool('start_scan', args)
            check(client.tool('start_scan', args)['job_id'] == started['job_id'], 'idempotency')
            job = client.wait(started['job_id'])
            check(job['state'] == 'partial' and job['progress']['unit'] == 'bytes', job)
            sid = job['result']['session_id']
            listed = client.tool('list_candidates', {'session_id':sid})
            check({c['id'] for c in listed['candidates'] if c['supported']} == set(truth), listed)
            job = client.wait(client.tool('recover_files', {'session_id':sid,'ids':list(truth),'destination':str(root/'mcp-out'),'request_id':'recover'})['job_id'])
            check(job['state'] == 'completed', job)
            check(all(Path(r['path']).read_bytes() == truth[r['candidate_id']] for r in job['result']['results']), 'MCP byte mismatch')
            cases += 1
        finally:
            client.close()
    print(f'PNG carving: {cases} cases; {len(variants)+len(truth)} byte-exact files; CLI/MCP passed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    main(parser.parse_args().binary.resolve())
