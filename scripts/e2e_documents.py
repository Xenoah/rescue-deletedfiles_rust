"""Independent encoded corpus + adversarial PDF/ZIP/Office/MP4/MOV carving.

Only stdlib is required to run on each native OS. Real corpus files were made and
reopened by ReportLab/PyMuPDF, python-docx/openpyxl/python-pptx and FFmpeg.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import struct
import tempfile
import zipfile
from e2e import Client, check, run

CORPUS = Path(__file__).resolve().parent.parent / 'tests/corpus'


def pdf(objects=None, extra=b''):
    objects = objects or [b'<< /Type /Catalog /Pages 2 0 R >>',
                          b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
                          b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 50] /Contents 4 0 R >>',
                          b'<< /Length 5 0 R >>\nstream\nBT (%%EOF inside stream) Tj ET\nendstream',
                          b'30']
    data = bytearray(b'%PDF-1.7\n'); offsets = []
    for n, value in enumerate(objects, 1):
        offsets.append(len(data)); data += f'{n} 0 obj\n'.encode()+value+b'\nendobj\n'
    xref = len(data)
    data += f'xref\n0 {len(objects)+1}\n0000000000 65535 f \n'.encode()
    data += b''.join(f'{offset:010d} 00000 n \n'.encode() for offset in offsets)
    data += f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R '.encode()+extra+b'>>\nstartxref\n'+str(xref).encode()+b'\n%%EOF\n'
    return bytes(data)


def archive(entries, method=zipfile.ZIP_DEFLATED, descriptor=False, zip64=False, comment=b''):
    class Pipe(io.BytesIO):
        def seekable(self): return False
        def seek(self, *a): raise io.UnsupportedOperation('pipe')
    out = Pipe() if descriptor else io.BytesIO()
    with zipfile.ZipFile(out, 'w', method) as z:
        for name, data in entries:
            if zip64:
                info = zipfile.ZipInfo(name); info.compress_type = method
                with z.open(info, 'w', force_zip64=True) as entry: entry.write(data)
            else: z.writestr(name, data)
        z.comment = comment
    return out.getvalue()


def changed(data, at, value):
    return data[:at]+value+data[at+len(value):]


def boxes(data):
    p = 0
    while p < len(data):
        n = int.from_bytes(data[p:p+4], 'big'); check(n >= 8, 'fixture box size')
        yield p, data[p+4:p+8], data[p:p+n]; p += n


def rewrite_box(data, kind, transform):
    output = bytearray()
    for _, tag, box in boxes(data):
        if tag == kind: box = transform(box)
        elif tag in [b'moov', b'trak', b'mdia', b'minf', b'stbl']:
            payload = rewrite_box(box[8:], kind, transform); box = struct.pack('>I', len(payload)+8)+tag+payload
        output += box
    return bytes(output)


def make_variants():
    manifest = json.loads((CORPUS/'manifest.json').read_text(encoding='utf-8'))
    entries = manifest['files']
    selected = ['pdf', 'zip', 'docx', 'xlsx', 'pptx', 'mp4', 'mov']
    real = {}
    for ext in selected:
        entry = next(e for e in entries if e['extension'] == ext)
        data = (CORPUS/entry['file']).read_bytes()
        check(hashlib.sha256(data).hexdigest() == entry['sha256'], 'corpus hash '+ext)
        real[ext] = data
    good = {('real-'+ext, ext): data for ext, data in real.items()}
    basic = pdf(); good[('indirect-stream-length', 'pdf')] = basic
    prev = int(basic.split(b'startxref\n')[-1].splitlines()[0]); offset = len(basic)
    update = b'3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>\nendobj\n'
    incremental = basic+update+f'xref\n3 1\n{offset:010d} 00000 n \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {prev} >>\nstartxref\n{len(basic)+len(update)}\n%%EOF\n'.encode()
    good[('incremental-pdf', 'pdf')] = incremental
    for method, name in [(0, 'stored'), (8, 'deflated')]:
        for descriptor in [False, True]:
            for wide in [False, True]:
                good[(f'zip-{name}-descriptor{descriptor}-zip64{wide}', 'zip')] = archive([('日本語.txt', b'known bytes '*4096), ('folder/', b'')], method, descriptor, wide, b'comment PK\x05\x06 with fake marker')
    good[('empty-zip', 'zip')] = archive([], comment=b'empty archive')
    z = archive([('one.txt', b'abc')]); eo = z.index(b'PK\x05\x06')
    central_size, central_offset = struct.unpack_from('<II', z, eo+12)
    end64 = struct.pack('<4sQHHIIQQQQ', b'PK\x06\x06', 44, 45, 45, 0, 0, 1, 1, central_size, central_offset)
    locator = struct.pack('<4sIQI', b'PK\x06\x07', 0, eo, 1)
    final = bytearray(z[eo:]); struct.pack_into('<HHII', final, 8, 65535, 65535, 0xffffffff, 0xffffffff)
    good[('zip64-end-records', 'zip')] = z[:eo]+end64+locator+final
    # Matching words in comments or a wrong XML namespace must not classify Office.
    fake_types = b'<Types xmlns="wrong"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'
    good[('false-office-namespace', 'zip')] = archive([('[Content_Types].xml', fake_types), ('word/document.xml', b'not XML')])
    good[('malformed-office-xml', 'zip')] = archive([('[Content_Types].xml', fake_types.replace(b'wrong', b'http://schemas.openxmlformats.org/package/2006/content-types')[:-8]), ('word/document.xml', b'x')])
    def co64(box):
        count = struct.unpack_from('>I', box, 12)[0]
        offsets = struct.unpack_from('>'+str(count)+'I', box, 16)
        payload = box[8:16]+b''.join(struct.pack('>Q', n) for n in offsets)
        return struct.pack('>I', len(payload)+8)+b'co64'+payload
    good[('mp4-co64', 'mp4')] = rewrite_box(real['mp4'], b'stco', co64)
    parts = list(boxes(real['mp4'])); moov = next(b for _, t, b in parts if t == b'moov')
    def shift(box):
        count = struct.unpack_from('>I', box, 12)[0]; values = struct.unpack_from('>'+str(count)+'I', box, 16)
        return box[:16]+b''.join(struct.pack('>I', n+len(moov)) for n in values)
    head = parts[0][2]; moved = rewrite_box(moov, b'stco', shift)
    good[('mp4-faststart', 'mp4')] = head+moved+b''.join(b for _, t, b in parts[1:] if t != b'moov')
    bad = {
        'pdf-no-eof': basic[:-8],
        'pdf-wrong-page-count': basic.replace(b'/Count 1', b'/Count 2'),
        'pdf-page-cycle': basic.replace(b'/Kids [3 0 R]', b'/Kids [2 0 R]'),
        'pdf-dangling-reference': basic.replace(b'/Contents 4 0 R', b'/Contents 9 0 R'),
        'pdf-bad-stream-length': basic.replace(b'5 0 obj\n30', b'5 0 obj\n29'),
        'pdf-bad-object-identity': basic.replace(b'3 0 obj', b'9 0 obj'),
        'pdf-encrypted': pdf(extra=b'/Encrypt 1 0 R '),
        'pdf-duplicate-root-key': pdf(extra=b'/Root 1 0 R '),
        'pdf-incomplete-update': basic+update,
        'pdf-bad-incremental': incremental.replace(f'/Prev {prev}'.encode(), b'/Prev 9999999'),
    }
    small = archive([('a.txt', b'known payload')], method=0); central = small.index(b'PK\x01\x02'); eo = small.index(b'PK\x05\x06')
    payload = 30+int.from_bytes(small[26:28], 'little')
    bad.update({
        'zip-no-end': small[:eo],
        'zip-wrong-crc': changed(small, payload, b'X'),
        'zip-central-offset': changed(small, eo+16, struct.pack('<I', central+1)),
        'zip-local-name': changed(small, 30, b'b'),
        'zip-local-size': changed(small, 18, struct.pack('<I', 1)),
        'zip-central-overflow': changed(small, central+24, struct.pack('<I', 0x7fffffff)),
        'zip-split': changed(small, eo+4, struct.pack('<H', 1)),
        'zip-encrypted': changed(changed(small, 6, b'\x01\x00'), central+8, b'\x01\x00'),
        'zip-entry-points-inside': changed(small, central+42, struct.pack('<I', 1)),
        'zip-overcount': changed(changed(small, eo+8, b'\x02\x00'), eo+10, b'\x02\x00'),
    })
    movie = real['mp4']; stco = movie.index(b'stco'); stsz = movie.index(b'stsz'); stts = movie.index(b'stts'); stsc = movie.index(b'stsc')
    moov_at = next(p for p, t, _ in parts if t == b'moov'); mdat_at = next(p for p, t, _ in parts if t == b'mdat')
    bad.update({
        'mp4-no-moov': movie[:moov_at],
        'mp4-cut-moov': movie[:-1],
        'mp4-offset-outside-media': changed(movie, stco+12, struct.pack('>I', 1)),
        'mp4-sample-count-bomb': changed(movie, stsz+12, b'\xff'*4),
        'mp4-timing-count': changed(movie, stts+12, struct.pack('>I', 999)),
        'mp4-map-first-chunk': changed(movie, stsc+12, struct.pack('>I', 2)),
        'mp4-zero-box-size': changed(movie, mdat_at, bytes(4)),
        'mp4-huge-box-size': changed(movie, moov_at, b'\xff'*4),
        'mp4-unknown-brand': changed(movie, 8, b'avif'),
        'mp4-missing-media': b''.join(b for _, t, b in parts if t != b'mdat'),
    })
    return good, bad


def main(binary, output=None):
    good, bad = make_variants(); cases = 0; exact = 0
    with tempfile.TemporaryDirectory(prefix='rescue-documents-') as temporary:
        root = Path(temporary)
        def scan(data, label, *args, expected=0):
            source = root/(label+'.raw'); source.write_bytes(data)
            before = hashlib.sha256(data).hexdigest()
            result = run(binary, 'scan', '--source', source, '--mode', 'carve', '--work-dir', root/(label+'-work'), '--json', *args, expected=expected)
            session = json.loads(Path(result['session']).read_text(encoding='utf-8'))
            check(hashlib.sha256(source.read_bytes()).hexdigest() == before, 'source changed')
            return result, session, source
        for (name, ext), data in good.items():
            result, session, source = scan(data, name)
            top = next(c for c in session['candidates'] if c['record'] == 0)
            check(top['supported'] and top['id'] == ext+'-0000000000000000' and top['size'] == len(data), (name, top))
            check(session['schema_version'] == 7 and not top['path_verified'] and top['allocation_status'] == 'unknown', name)
            restored = run(binary, 'recover', '--session', result['session'], '--id', top['id'], '--destination', root/(name+'-out'), '--json')['results'][0]
            check(restored['status'] == 'completed' and Path(restored['path']).read_bytes() == data, (name, restored)); exact += 1; cases += 1
        for name, data in bad.items():
            result, session, _ = scan(data, name, expected=5)
            top = next(c for c in session['candidates'] if c['record'] == 0)
            check(not top['supported'], (name, top))
            restored = run(binary, 'recover', '--session', result['session'], '--id', top['id'], '--destination', root/(name+'-out'), '--json', expected=5)['results'][0]
            check(restored['path'] is None, (name, restored)); cases += 1
        # Every new header crosses the scan-window boundary, with an exact range and one-byte cut.
        for ext in ['pdf', 'zip', 'docx', 'xlsx', 'pptx', 'mp4', 'mov']:
            data = good[('real-'+ext, ext)]; at = 1024*1024-3
            image = bytes(at)+data+bytes(71)
            result, session, _ = scan(image, 'boundary-'+ext, '--extension', ext)
            selected = [c for c in session['candidates'] if c['supported']]
            check(len(selected) == 1 and selected[0]['record'] == at, (ext, selected)); cases += 1
            result, session, _ = scan(data[:-1], 'cut-'+ext, '--extension', ext, expected=0 if ext == 'pdf' else 5)
            # PDF's final newline is optional; its ambiguous trailing padding is documented.
            check(bool([c for c in session['candidates'] if c['supported']]) == (ext == 'pdf'), ext); cases += 1
        nested = archive([('inner.zip', archive([('text.txt', b'inside')], method=0))], method=0)
        result, session, _ = scan(nested, 'nested')
        check(len([c for c in session['candidates'] if c['supported']]) == 2, 'nested ZIP must remain visible'); cases += 1
        # A single mixed source tests the same parser through the actual stdio MCP server.
        truth = {}; data = bytearray()
        for ext in ['pdf', 'zip', 'docx', 'xlsx', 'pptx', 'mp4', 'mov']:
            item = good[('real-'+ext, ext)]; truth[f'{ext}-{len(data):016x}'] = item; data += item+bytes(31)
        result, session, source = scan(data, 'mixed')
        check({c['id'] for c in session['candidates'] if c['supported']} == set(truth), 'mixed candidates')
        client = Client(binary, source, root/'mcp-work', root/'mcp-out')
        try:
            client.rpc('initialize', {'protocolVersion': '2025-11-25'})
            job = client.wait(client.tool('start_scan', {'source': str(source), 'mode': 'carve', 'request_id': 'documents'})['job_id'])
            check(job['state'] == 'completed', job); sid = job['result']['session_id']
            candidates = client.tool('list_candidates', {'session_id': sid})['candidates']
            check({c['id'] for c in candidates if c['supported']} == set(truth), candidates)
            job = client.wait(client.tool('recover_files', {'session_id': sid, 'ids': list(truth), 'destination': str(root/'mcp-out'), 'request_id': 'restore-documents'})['job_id'])
            check(job['state'] == 'completed', job)
            for item in job['result']['results']:
                check(Path(item['path']).read_bytes() == truth[item['candidate_id']], item); exact += 1
        finally: client.close()
        cases += 1
    report = {'schema_version': 1, 'platform': platform.platform(), 'cases': cases, 'byte_exact_outputs': exact,
              'valid_variants': len(good), 'rejected_corrupt_or_unsupported': len(bad), 'interfaces': ['cli', 'stdio_mcp'], 'status':'passed',
              'version': __import__('subprocess').run([str(binary),'--version'],check=True,capture_output=True,text=True).stdout.strip(),
              'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'Synthetic fixtures; structural PDF/movie validation, ZIP CRC. No claim of original integrity without known bytes.'}
    if output: Path(output).write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(report, ensure_ascii=False))
    return report


if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--binary', type=Path, required=True); p.add_argument('--output', type=Path)
    a = p.parse_args(); main(a.binary.resolve(), a.output)
