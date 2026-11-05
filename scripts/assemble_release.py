"""Require all four native package validations before assembling release assets."""
import base64
import hashlib
import json
from pathlib import Path
import re
import shutil
from package import ROOT, VERSION

dist=ROOT/'dist'
for platform,arch in [('windows','x64'),('linux','x64'),('macos','x64'),('macos','arm64')]:
    for prefix in ['package','verification']:
        data=json.loads((dist/f'{prefix}-{platform}-{arch}.json').read_text())
        if data['version']!=VERSION or (prefix=='verification' and data['status']!='passed'):raise SystemExit('Unverified or mismatched distribution')
    disk=json.loads((dist/f'disk-results-{platform}-{arch}.json').read_text())
    if disk['version']!=f'rescue {VERSION}' or disk['status']!='passed' or not disk['native_virtual_disk'] or disk['byte_exact']!=864:raise SystemExit('Native disk recovery proof mismatch')
    restore=json.loads((dist/f'restore-native-{platform}-{arch}.json').read_text())
    if restore['version']!=f'rescue {VERSION}' or restore['status']!='passed' or not all(restore[k] for k in ['native_same_volume','actual_deletion','original_root_auto_detected','cli_pdf_byte_exact','mcp_pdf_byte_exact','session_work_never_created']):raise SystemExit('Native original-location recovery proof mismatch')
    jpeg=json.loads((dist/f'jpeg-results-{platform}-{arch}.json').read_text())
    if jpeg['version']!=f'rescue {VERSION}' or jpeg['status']!='passed' or jpeg['corpus_jpegs']!=32 or jpeg['jpeg_candidates_exact']!=33 or jpeg['malformed_cases']!=33 or not jpeg['cli_mcp_byte_exact'] or not jpeg['source_unchanged']:raise SystemExit('JPEG carving proof mismatch')
    documents=json.loads((dist/f'document-results-{platform}-{arch}.json').read_text())
    if documents['version']!=f'rescue {VERSION}' or documents['status']!='passed' or documents['valid_variants']<23 or documents['rejected_corrupt_or_unsupported']<30:raise SystemExit('Document carving proof mismatch')
    ntfs=json.loads((dist/f'ntfs-results-{platform}-{arch}.json').read_text());assert ntfs['version']==f'rescue {VERSION}' and ntfs['status']=='passed' and ntfs['checks']>=22
    if platform=='windows':
        native=json.loads((dist/'ntfs-native-windows-x64.json').read_text());assert native['version']==f'rescue {VERSION}' and native['status']=='passed' and native['actual_deletion'] and native['cli_streams_byte_exact']==21 and native['mcp_streams_byte_exact']==21
    image=json.loads((dist/f'imaging-results-{platform}-{arch}.json').read_text());assert image['version']==f'rescue {VERSION}' and image['status']=='passed' and image['checks']>=15
    image_native=json.loads((dist/f'imaging-native-{platform}-{arch}.json').read_text());assert image_native['version']==f'rescue {VERSION}' and image_native['status']=='passed' and image_native['native_permission_denied'] and image_native['resume_after_capacity_failure_byte_exact'] and image_native['source_unchanged']
    assert disk['native_access']['acquisition_byte_exact'] and disk['native_access']['acquired_files_byte_exact']==108
    checksum_file=dist/f'SHA256SUMS-{platform}-{arch}.txt'
    for line in checksum_file.read_text().splitlines():
        expected,name=line.split('  ',1)
        if Path(name).name!=name or hashlib.sha256((dist/name).read_bytes()).hexdigest()!=expected:raise SystemExit('Native checksum mismatch: '+name)
proof=json.loads((ROOT/'docs/ui-flow-results.json').read_text())
if proof['application_version']!=VERSION or proof['files_byte_exact']!=36:raise SystemExit('GUI flow proof mismatch')
for shot in proof['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('GUI screenshot proof mismatch')
shutil.copy2(ROOT/'docs/ui-flow-results.json',dist/'gui-flow-results.json')
original_gui=json.loads((ROOT/'docs/gui-original-results.json').read_text())
if original_gui['application_version']!=VERSION or original_gui['original_files_byte_exact']!=36 or not original_gui['existing_pdf_preserved'] or not original_gui['source_unchanged']:raise SystemExit('GUI original-location proof mismatch')
shutil.copy2(ROOT/'docs/gui-original-results.json',dist/'gui-original-results.json')
shutil.copy2(ROOT/'docs/ui-demo-manifest.json',dist/'gui-demo-manifest.json')
jpeg_gui=json.loads((ROOT/'docs/gui-jpeg-results.json').read_text())
if jpeg_gui['application_version']!=VERSION or jpeg_gui['jpeg_files_byte_exact']!=33 or not jpeg_gui['source_unchanged']:raise SystemExit('JPEG GUI proof mismatch')
for shot in jpeg_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('JPEG screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-jpeg-results.json',dist/'gui-jpeg-results.json')
document_gui=json.loads((ROOT/'docs/gui-document-results.json').read_text())
if document_gui['application_version']!=VERSION or document_gui['document_files_byte_exact']!=7 or not document_gui['source_unchanged']:raise SystemExit('Document GUI proof mismatch')
for shot in document_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('Document screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-document-results.json',dist/'gui-document-results.json')
imaging_gui=json.loads((ROOT/'docs/gui-imaging-results.json').read_text())
if imaging_gui['application_version']!=VERSION or imaging_gui['source_bytes']!=512*1024*1024 or imaging_gui['copied_ranges']!=128 or imaging_gui['recovered_files_byte_exact']!=36 or imaging_gui['gui_map_pages_exercised']<2 or not all(imaging_gui[k] for k in ['image_source_byte_exact','source_unchanged','map_digest_unchanged_after_gui_resume','zip_crc_verified']):raise SystemExit('Imaging GUI proof mismatch')
for shot in imaging_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('Imaging screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-imaging-results.json',dist/'gui-imaging-results.json')
html=(ROOT/'docs/intelligence.html').read_text(encoding='utf-8')
def embedded(match):
    path=ROOT/'docs'/match.group(1)
    return 'src="data:image/png;base64,'+base64.b64encode(path.read_bytes()).decode('ascii')+'"'
html=re.sub(r'src="(images/[^"<>]+\.png)"',embedded,html)
(dist/'intelligence.html').write_text(html,encoding='utf-8')
for name in ['details-view.png','settings-view.png','icons-view.png','technical-ui-v11.png','imaging-settings.png','imaging-progress.png','read-map.png','jpeg-carving.png','jpeg-restored.png','document-carving.png','document-restored.png','small-window.png','disk-selection.png','original-location.png','original-restored.png']:
    shutil.copy2(ROOT/'docs/images'/name,dist/name)
assets=sorted(path for path in dist.iterdir() if path.is_file() and path.name!='SHA256SUMS.txt')
(dist/'SHA256SUMS.txt').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in assets),encoding='ascii')
print(f'All four native packages verified; {len(assets)} release assets checksummed.')
