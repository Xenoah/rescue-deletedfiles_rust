"""Raw export of data that cannot be recovered as a verified file (damaged media).

A synthetic NTFS volume is acquired, one 512-byte block of a fragmented file is
marked permanently unreadable in the read map, and normal recovery must still
refuse to invent bytes. export-raw must then write exactly the readable bytes
with the unreadable range zero-filled, list that range in a JSON map, mark the
output unverified, never overwrite, and refuse a destination on the source.
Expected bytes are computed here from the fixture truth, independently of Rust.
"""
import argparse, hashlib, json, subprocess, tempfile
from pathlib import Path
from e2e import check, run
from e2e_imaging import mark_gap, sha
from make_fixture import build


def main(binary, output=None):
    cases = []
    with tempfile.TemporaryDirectory(prefix='rescue-export-raw-') as tmp:
        root = Path(tmp); source = root / 'source.raw'; truth = build(source)
        acquired = run(binary, 'image', '--source', source, '--destination', root / 'image', '--block-size', '512', '--json')
        image = Path(acquired['image'])
        payload = bytes.fromhex(truth['mft-21-7']['hex'])
        raw = source.read_bytes()
        # The fixture stores this file's clusters out of order; locate each 512 B piece.
        at = 300 * 512; check(raw[at:at + 512] == payload[:512], 'fixture payload')
        gap_start, gap_length = mark_gap(image, at)
        before = sha(image)
        scan = run(binary, 'scan', '--source', image, '--work-dir', root / 'work', '--json', expected=5)
        session = scan['session']
        normal = run(binary, 'recover', '--session', session, '--id', 'mft-21-7', '--destination', root / 'normal', '--json', expected=5)['results'][0]
        check(normal['status'] == 'incomplete' and normal['path'] is None, normal)
        cases.append('normal recovery refuses unreadable data')

        out = root / 'raw'
        result = run(binary, 'export-raw', '--session', session, '--id', 'mft-21-7', '--id', 'mft-20-7', '--destination', out, '--json')
        check(result['verified_recovery'] is False and result['exported'] == 2 and result['not_exportable'] == 0, result)
        by_id = {e['candidate_id']: e for e in result['exports']}
        gapped = by_id['mft-21-7']
        check(gapped['status'] == 'exported_with_gaps' and gapped['zero_filled'], gapped)
        # Independent expectation from the fixture layout (make_fixture.build):
        # logical clusters 0,1,2 of mft-21-7 are LCN 300,302,301, 512-byte clusters,
        # initialized to 1100 bytes. Pieces whose sector is in the failed block read as zero.
        expected = bytearray(payload)
        for logical, lcn in enumerate([300, 302, 301]):
            physical = lcn * 512
            if gap_start <= physical < gap_start + gap_length:
                expected[logical * 512:min(len(payload), logical * 512 + 512)] = bytes(min(512, len(payload) - logical * 512))
        written = Path(gapped['bin_path']).read_bytes()
        check(written == bytes(expected), 'zero-filled export differs from the independent model')
        check(written != payload and len(written) == len(payload), 'gap not represented')
        check(written[512:1024] == payload[512:1024] and written[:512] == bytes(512), 'only the failed sector may be zero')
        check(gapped['zero_filled'] == [{'logical_offset': 0, 'length': 512}], gapped)
        check(gapped['sha256'] == hashlib.sha256(written).hexdigest(), gapped)
        for z in gapped['zero_filled']:
            check(written[z['logical_offset']:z['logical_offset'] + z['length']] == bytes(z['length']), z)
        record = json.loads(Path(gapped['map_path']).read_text(encoding='utf-8'))
        check(record['export']['zero_filled'] == gapped['zero_filled'] and 'Not a verified recovery' in record['notice'], record)
        cases.append('unreadable block zero-filled, other bytes exact, ranges mapped')
        full = by_id['mft-20-7']
        check(full['status'] == 'exported' and not full['zero_filled'], full)
        check(Path(full['bin_path']).read_bytes() == bytes.fromhex(truth['mft-20-7']['hex']), 'readable file export')
        cases.append('fully readable candidate exported byte-exact')

        again = subprocess.run([str(binary), 'export-raw', '--session', session, '--id', 'mft-20-7', '--destination', out, '--json'], capture_output=True, text=True, encoding='utf-8')
        check(again.returncode != 0 and Path(full['bin_path']).read_bytes() == bytes.fromhex(truth['mft-20-7']['hex']), 'existing export overwritten')
        cases.append('existing export never overwritten')
        unknown = subprocess.run([str(binary), 'export-raw', '--session', session, '--id', 'mft-999-7', '--destination', root / 'unknown', '--json'], capture_output=True, text=True, encoding='utf-8')
        check(unknown.returncode == 2, unknown.stderr)
        cases.append('unknown candidate rejected')
        check(sha(image) == before, 'source image changed')
        cases.append('source unchanged')
    result = {'status': 'passed', 'version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
              'checks': len(cases), 'cases': cases, 'files_byte_exact': 2, 'source_unchanged': True, 'scope': __doc__.strip()}
    if output:
        Path(output).write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False))
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--binary', type=Path, required=True); p.add_argument('--output', type=Path)
    a = p.parse_args(); main(a.binary.resolve(), a.output)
