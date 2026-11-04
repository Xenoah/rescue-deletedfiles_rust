"""Original paths and collisions through real CLI/MCP byte recovery.

Only new disposable images and directories are modified. Original paths in
serialized sessions are deliberately forged to ensure source metadata wins.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import tempfile
import zipfile
from e2e import Client, check, run
from make_fixture import build, record, filename, nonresident, resident, MFT_LCN, SECTOR, RECORD
from make_ui_fixture import build as build_ui

ROOT = Path(__file__).resolve().parent.parent


def main(binary, output=None):
    checks = 0
    with tempfile.TemporaryDirectory(prefix='rescue-restore-') as temporary:
        root = Path(temporary)
        source = root/'source.raw'
        truth = build_ui(source)
        original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
        pdf = (ROOT/'tests/corpus/files/sample.pdf').read_bytes()
        scan = run(binary, 'scan', '--source', source, '--work-dir', root/'work', '--json')
        session = Path(scan['session'])
        candidates = run(binary, 'results', '--session', session, '--limit', '1000', '--json')['candidates']
        candidate = next(c for c in candidates if c['name'] == 'sample.pdf')
        check(candidate['path_verified'] and candidate['original_path'] == '01_書類/sample.pdf', candidate)
        checks += 1
        long_source=root/'long-name.raw'
        build(long_source)
        long_name='long-'+('x'*180)+'.txt'
        with long_source.open('r+b') as f:
            f.seek(MFT_LCN*SECTOR+23*RECORD)
            f.write(record(23,[filename(long_name),resident(0x80,b'preserved long filename')]))
        long=run(binary,'restore','--source',long_source,'--name',long_name,'--destination',root/'long-output','--json')['results'][0]
        check(Path(long['path']).name==long_name and Path(long['path']).read_bytes()==b'preserved long filename',long)
        duplicate=run(binary,'restore','--source',long_source,'--name',long_name,'--destination',root/'long-output','--json')['results'][0]
        check(Path(duplicate['path']).name==long_name[:-4]+' (復元 1).txt' and Path(long['path']).read_bytes()==b'preserved long filename',duplicate)
        checks+=2
        original = root/'original-volume'
        documents = original/'01_書類'
        documents.mkdir(parents=True)
        existing = documents/'sample.pdf'
        existing.write_bytes(b'EXISTING PDF: KEEP THIS')
        # A one-command accidental-deletion workflow: no persisted scan needed.
        recovered = run(binary, 'restore', '--source', source, '--name', 'SAMPLE.PDF', '--original',
                        '--volume-root', original, '--json')['results'][0]
        check(recovered['status'] == 'completed' and Path(recovered['path']).name == 'sample (復元 1).pdf', recovered)
        check(Path(recovered['path']).read_bytes() == pdf and existing.read_bytes() == b'EXISTING PDF: KEEP THIS', 'PDF collision damaged data')
        checks += 3
        duplicate = run(binary, 'recover', '--session', session, '--id', candidate['id'], '--original',
                        '--volume-root', original, '--json')['results'][0]
        check(Path(duplicate['path']).name == 'sample (復元 2).pdf' and Path(duplicate['path']).read_bytes() == pdf, duplicate)
        checks += 1
        skipped = run(binary, 'recover', '--session', session, '--id', candidate['id'], '--original',
                      '--volume-root', original, '--collision', 'skip', '--json')['results'][0]
        check(skipped['status'] == 'skipped' and skipped['bytes_written'] == 0 and existing.read_bytes() == b'EXISTING PDF: KEEP THIS', skipped)
        checks += 1
        # Missing mount mappings are explicit failures, never guessed locations.
        no_root = run(binary, 'recover', '--session', session, '--id', candidate['id'], '--original', '--json', expected=5)
        check(no_root['results'][0]['path'] is None and no_root['results'][0]['status'] == 'failed', no_root)
        checks += 1
        # Filenames and directory paths from saved JSON cannot choose a write path.
        edited = json.loads(session.read_text(encoding='utf-8'))
        forged = next(c for c in edited['candidates'] if c['id'] == candidate['id'])
        forged['original_path'] = '../outside/owned.pdf'
        forged['name'] = 'owned.pdf'
        forged['size'] = 2**64-1
        forged['path_verified'] = True
        tampered = root/'tampered.json'
        tampered.write_text(json.dumps(edited), encoding='utf-8')
        output_folder = root/'normal-output'
        result = run(binary, 'recover', '--session', tampered, '--id', candidate['id'], '--destination', output_folder, '--json')['results'][0]
        check(Path(result['path']).parent.samefile(output_folder/'01_書類') and Path(result['path']).name=='sample.pdf', result)
        check(Path(result['path']).read_bytes() == pdf and not (root/'outside').exists(), 'forged output escaped')
        checks += 2
        # A valid sparse stream over the batch limit is rejected before any
        # output directory or report is created, without allocating its payload.
        huge_source=root/'large-sparse.raw'
        build(huge_source)
        size=513*1024*1024
        with huge_source.open('r+b') as f:
            f.seek(MFT_LCN*SECTOR+23*RECORD)
            f.write(record(23,[filename('memory-limit.bin'),nonresident([(size//SECTOR,None)],size,flags=0x8000)]))
        huge_session=run(binary,'scan','--source',huge_source,'--work-dir',root/'huge-work','--json')['session']
        rejected=run(binary,'recover','--session',huge_session,'--id','mft-23-7','--destination',root/'must-not-create','--json',expected=4)
        check('RESOURCE_LIMIT' in str(rejected) and not (root/'must-not-create').exists(),rejected)
        checks += 1
        # Merge existing directories. A file blocking a directory is preserved,
        # and all siblings share one renamed recovered directory.
        merged = root/'merged-volume'
        merged.mkdir()
        blocked = merged/'01_書類'
        blocked.write_bytes(b'EXISTING FILE WHERE FOLDER SHOULD BE')
        images = merged/'02_画像'
        images.mkdir()
        (images/'keep.txt').write_bytes(b'KEEP')
        result = run(binary, 'recover', '--session', session, '--all', '--original', '--volume-root', merged, '--json')['results']
        check(len(result) == 36 and all(r['status'] == 'completed' for r in result), result)
        for r in result:
            check(hashlib.sha256(Path(r['path']).read_bytes()).hexdigest() == truth['files'][r['candidate_id']]['sha256'], r)
        document_paths = [Path(r['path']).parent for r in result if Path(r['path']).suffix.lower() in ['.pdf', '.docx', '.xlsx', '.pptx']]
        check(len(set(document_paths)) == 1 and document_paths[0].name == '01_書類 (復元 1)', document_paths)
        check(blocked.read_bytes() == b'EXISTING FILE WHERE FOLDER SHOULD BE' and (images/'keep.txt').read_bytes() == b'KEEP', 'merge clobbered content')
        checks += 39
        # Case collisions behave consistently on case-sensitive filesystems.
        case_root = root/'case-volume'
        (case_root/'01_書類').mkdir(parents=True)
        (case_root/'01_書類/SAMPLE.PDF').write_bytes(b'UPPERCASE EXISTING')
        case = run(binary, 'restore', '--source', source, '--name', 'sample.pdf', '--original', '--volume-root', case_root, '--json')['results'][0]
        check(Path(case['path']).name == 'sample (復元 1).pdf', case)
        checks += 1
        # A linked original subdirectory is never followed into another tree.
        links = root/'links-volume'
        links.mkdir()
        outside = root/'linked-outside'
        outside.mkdir()
        try:
            (links/'01_書類').symlink_to(outside, target_is_directory=True)
        except OSError:
            check(os.name == 'nt', 'unexpected symlink creation failure')
        else:
            link = run(binary, 'restore', '--source', source, '--name', 'sample.pdf', '--original', '--volume-root', links, '--json')['results'][0]
            check(Path(link['path']).parent.name == '01_書類 (復元 1)' and not list(outside.iterdir()), link)
            checks += 1
        # Unverified parent references can be saved to a chosen folder, but not
        # used for original-location writes.
        unknown_source = root/'unknown.raw'
        build(unknown_source)
        unknown = run(binary, 'restore', '--source', unknown_source, '--name', 'stale-parent.txt', '--original', '--volume-root', original, '--json', expected=5)['results'][0]
        check(unknown['status'] == 'failed' and unknown['path'] is None, unknown)
        fallback = run(binary, 'restore', '--source', unknown_source, '--name', 'stale-parent.txt', '--destination', root/'fallback', '--json')['results'][0]
        check(fallback['status'] == 'completed', fallback)
        checks += 2
        # Same-volume ZIP naming, independent CRC checks and byte oracle.
        archive = original/'selected.zip'
        archive.write_bytes(b'EXISTING ZIP')
        zipped = run(binary, 'recover', '--session', session, '--id', candidate['id'], '--archive', archive, '--json')['result']
        check(Path(zipped['archive']).name == 'selected (復元 1).zip' and archive.read_bytes() == b'EXISTING ZIP', zipped)
        with zipfile.ZipFile(zipped['archive']) as z:
            check(z.testzip() is None and z.read('01_書類/sample.pdf') == pdf, 'ZIP bytes or CRC mismatch')
        checks += 2
        # MCP permits original-location output only inside its pinned allowlist.
        mcp_volume = root/'mcp-volume'
        mcp_docs = mcp_volume/'01_書類'
        mcp_docs.mkdir(parents=True)
        with closing(Client(binary, source, root/'mcp-work', mcp_docs)) as client:
            client.rpc('initialize', {'protocolVersion': '2025-11-25'})
            scanned = client.wait(client.tool('start_scan', {'source': str(source), 'request_id': 'scan-original'})['job_id'])
            sid = scanned['result']['session_id']
            args = {'session_id': sid, 'ids': [candidate['id']], 'output_format': 'original',
                    'volume_roots': [{'path': str(mcp_volume)}], 'request_id': 'restore-original'}
            restored = client.wait(client.tool('recover_files', args)['job_id'])
            check(restored['state'] == 'completed' and Path(restored['result']['results'][0]['path']).read_bytes() == pdf, restored)
            args['request_id'] = 'skip-original'
            args['collision'] = 'skip'
            skipped = client.wait(client.tool('recover_files', args)['job_id'])
            check(skipped['state'] == 'completed' and skipped['result']['results'][0]['status'] == 'skipped', skipped)
            args['request_id'] = 'deny-original'
            args['volume_roots'] = [{'path': str(outside)}]
            denied = client.wait(client.tool('recover_files', args)['job_id'])
            check(denied['state'] == 'partial' and denied['result']['results'][0]['path'] is None and not list(outside.iterdir()), denied)
            checks += 3
        check(hashlib.sha256(source.read_bytes()).hexdigest() == original_hash, 'read-only source image changed')
        checks += 1
    proof = {'schema_version': 1, 'status': 'passed', 'checks': checks,
             'scope': 'CLI/MCP original paths, PDF bytes, 36-file folder merge, keep-both/skip, case and link collisions, untrusted sessions, ZIP CRC; disposable regular images'}
    if output:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(proof, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(proof))


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path)
    a = p.parse_args()
    main(a.binary.resolve(), a.output)
