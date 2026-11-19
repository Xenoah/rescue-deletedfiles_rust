"""Recover independent fixed-length and delimited records through CLI and MCP."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile

from e2e import Client, check, run


def verify(binary):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        image = root / 'vendor.raw'
        # Header straddles the scan window; footer straddles the parser's read window.
        offset = 1024 * 1024 - 2
        original = b'VEND' + b'x' * (65536 - 1) + b'END!'
        body = bytes(offset) + original + b'after the file'
        image.write_bytes(body)
        rule = 'vendor:56454e44:end=454e4421'
        fixed = 'vendor:56454e44:size=' + str(len(original))

        def scan(label, rule, *options, expected=0):
            return run(binary, 'scan', '--source', image, '--mode', 'carve',
                       '--signature-rule', rule, '--work-dir', root / label,
                       '--json', *options, expected=expected)
        for name, spec, flags in [('fixed', fixed, []), ('footer', rule, []),
                                  ('fixed-index', fixed, ['--indexed']), ('footer-index', rule, ['--indexed'])]:
            s = scan(name, spec, *flags)
            c, = run(binary, 'results', '--session', s['session'], '--json')['candidates']
            check(c['record'] == offset and c['extent_evidence'] == 'signature_rule' and c['size'] == len(original), c)
            result, = run(binary, 'recover', '--session', s['session'], '--id', c['id'],
                          '--destination', root / (name + '-out'), '--json')['results']
            check(Path(result['path']).read_bytes() == original and result['sha256'] == hashlib.sha256(original).hexdigest(), result)
            print(name, result['sha256'])
            if name == 'footer':
                path = Path(s['session'])
                session = json.loads(path.read_text(encoding='utf-8'))
                session['search']['signature_rule'] = fixed
                path.write_text(json.dumps(session), encoding='utf-8')
                bad = run(binary, 'recover', '--session', path, '--id', c['id'],
                          '--destination', root / 'changed-rule', '--json', expected=5)
                check(not bad['results'][0].get('path'), bad)
        for i, spec in enumerate([fixed, rule]):
            s = scan('short' + str(i), spec, '--range-end', str(offset + len(original) - 1), expected=5)
            c, = run(binary, 'results', '--session', s['session'], '--json')['candidates']
            check(not c['supported'], c)
        listed = scan('filtered', rule, '--extension', 'pdf')
        check(listed['candidates'] == 0, listed)
        client = Client(binary, image, root / 'mcp-work', root / 'mcp-out')
        try:
            client.rpc('initialize', {'protocolVersion': '2025-11-25'})
            job = client.wait(client.tool('start_scan', {'request_id': 'custom', 'source': str(image),
                              'mode': 'carve', 'signature_rule': rule, 'indexed': True})['job_id'])
            check(job['state'] == 'completed', job)
            sid = job['result']['session_id']
            c, = client.tool('list_candidates', {'session_id': sid})['candidates']
            job = client.wait(client.tool('recover_files', {'request_id': 'save-custom', 'session_id': sid,
                              'ids': [c['id']], 'destination': str(root / 'mcp-out')})['job_id'])
            check(job['state'] == 'completed' and Path(job['result']['results'][0]['path']).read_bytes() == original, job)
        finally:
            client.close()
        check(image.read_bytes() == body, 'source changed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    verify(parser.parse_args().binary.resolve())
