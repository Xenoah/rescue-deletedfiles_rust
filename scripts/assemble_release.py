"""Require all four native package validations before assembling release assets."""
import base64
import hashlib
import json
from pathlib import Path
import re
import shutil
from package import ROOT, VERSION

dist=ROOT/'dist'
index_failures=[]
# GUI recordings of flows that alpha.16 did not change may be carried from the build
# that recorded them. Each must be listed with that exact version, and README and the
# release notes state which screens are carried. New evidence must match VERSION.
carry=json.loads((ROOT/'docs/gui-evidence-carryover.json').read_text(encoding='utf-8'))
if carry['release']!=VERSION:raise SystemExit('GUI carry-over ledger is for another release')
def current(name,recorded):
    entry=carry['files'].get(name)
    return recorded==VERSION or (entry is not None and entry['recorded_version']==recorded)
contract_failures=[]
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
    ntfs=json.loads((dist/f'ntfs-results-{platform}-{arch}.json').read_text());assert ntfs['version']==f'rescue {VERSION}' and ntfs['status']=='passed' and ntfs['checks']>=27
    for prefix,minimum in [('discovery',20),('remnant',12)]:
        proof=json.loads((dist/f'{prefix}-results-{platform}-{arch}.json').read_text());assert proof['version']==f'rescue {VERSION}' and proof['status']=='passed' and proof['checks']>=minimum and proof['source_unchanged']
    raid=json.loads((dist/f'raid-results-{platform}-{arch}.json').read_text());assert raid['version']==f'rescue {VERSION}' and raid['status']=='passed' and raid['checks']>=22 and raid['logical_arrays_byte_exact']==44 and raid['files_byte_exact']==73 and raid['source_unchanged']
    if platform=='linux':
        native=json.loads((dist/'raid-native-linux-x64.json').read_text());assert native['version']==f'rescue {VERSION}' and native['status']=='passed' and native['arrays_created']==5 and native['logical_arrays_byte_exact']==12 and native['files_byte_exact']==72 and native['source_unchanged']
    if platform=='linux':
        proof=json.loads((dist/'quickformat-results-linux-x64.json').read_text());assert proof['version']==f'rescue {VERSION}' and proof['status']=='passed' and proof['actual_quickformat'] and not proof['metadata_edited'] and proof['cli_files_byte_exact']==100 and proof['mcp_files_byte_exact']==100
    if platform=='windows':
        native=json.loads((dist/'ntfs-native-windows-x64.json').read_text());assert native['version']==f'rescue {VERSION}' and native['status']=='passed' and native['actual_deletion'] and native['cli_streams_byte_exact']==21 and native['mcp_streams_byte_exact']==21 and native['resident_stream_profile'] and len(native['nonresident_profiles'])==2 and all(p['policy']['restored'] and (p['erased_metadata_not_fabricated'] or p['retained_streams_byte_exact']==21) for p in native['nonresident_profiles'])
    image=json.loads((dist/f'imaging-results-{platform}-{arch}.json').read_text());assert image['version']==f'rescue {VERSION}' and image['status']=='passed' and image['checks']>=15
    image_native=json.loads((dist/f'imaging-native-{platform}-{arch}.json').read_text());assert image_native['version']==f'rescue {VERSION}' and image_native['status']=='passed' and image_native['native_permission_denied'] and image_native['resume_after_capacity_failure_byte_exact'] and image_native['source_unchanged']
    assert disk['native_access']['acquisition_byte_exact'] and disk['native_access']['acquired_files_byte_exact']==108
    # Windows adds three native RtlCompressBuffer LZNT1 streams to the shared 13.
    for prefix,minimum,files in [('index',10 if platform=='windows' else 12,38),('index-carve',65,60),('index-fat',11 if platform=='windows' else 15,51),('index-remnants',8,16 if platform=='windows' else 13),('index-late',7,6),('index-discovery',6,76)]:
        proof=json.loads((dist/f'{prefix}-results-{platform}-{arch}.json').read_text())
        observed={key:proof.get(key) for key in ['version','status','checks','files_byte_exact','source_unchanged','source_bytes_unchanged']}
        if not (proof['version']==f'rescue {VERSION}' and proof['status']=='passed' and proof['checks']>=minimum and proof['files_byte_exact']==files and proof.get('source_unchanged',proof.get('source_bytes_unchanged',False))):
            index_failures.append(f'{prefix}/{platform}/{arch}: expected checks >= {minimum}, files == {files}, version rescue {VERSION}; observed {observed}')
    # alpha.16 suites: preview, allocation scope, job contract, naming and source identity.
    linux_minimum={'export-raw':(6,2),'damaged-fat':(5,20),'preview':(11,None),'allocation-scope':(19,24),'job-contract':(12,6),'naming':(14,13),'source-contract':(10,10)}
    for prefix,(checks,files) in linux_minimum.items():
        proof=json.loads((dist/f'{prefix}-results-{platform}-{arch}.json').read_text())
        ok=proof['version']==f'rescue {VERSION}' and proof['status']=='passed' and proof['source_unchanged'] and proof['checks']>0
        if platform=='linux':ok=ok and proof['checks']>=checks and (files is None or proof['files_byte_exact']==files)
        if not ok:contract_failures.append(f"{prefix}/{platform}/{arch}: observed {{'version':{proof.get('version')!r},'status':{proof.get('status')!r},'checks':{proof.get('checks')!r},'files':{proof.get('files_byte_exact')!r}}}")
    fuzz=json.loads((dist/f'fuzz-results-{platform}-{arch}.json').read_text())
    if not (fuzz['version']==f'rescue {VERSION}' and fuzz['status']=='passed' and fuzz['checks']>=400 and fuzz['panics']==0 and fuzz['source_unchanged']):contract_failures.append(f"fuzz/{platform}/{arch}: observed checks {fuzz.get('checks')!r}")
    if platform=='windows':
        for kind in ['native','limited']:
            proof=json.loads((dist/f'read-access-{kind}-windows-x64.json').read_text())
            if not (proof['version']==f'rescue {VERSION}' and proof['status']=='passed' and proof['source_unchanged'] and proof['files_byte_exact']>0):contract_failures.append(f'read-access-{kind}: observed status {proof.get("status")!r}')
            if kind=='limited' and not proof['unelevated_parent_tested']:contract_failures.append('read-access-limited: parent was elevated')
        uia=json.loads((dist/'gui-native-windows-x64.json').read_text())
        if not (uia['status']=='passed' and uia['files_byte_exact']==2 and uia['source_unchanged'] and len(uia['uia']['steps'])>=10):contract_failures.append(f"native Windows UIA: observed status {uia.get('status')!r}")
    checksum_file=dist/f'SHA256SUMS-{platform}-{arch}.txt'
    for line in checksum_file.read_text().splitlines():
        expected,name=line.split('  ',1)
        if Path(name).name!=name or hashlib.sha256((dist/name).read_bytes()).hexdigest()!=expected:raise SystemExit('Native checksum mismatch: '+name)
if index_failures:raise SystemExit('Native index proof mismatches:\n'+'\n'.join(index_failures))
if contract_failures:raise SystemExit('Native alpha.16 proof mismatches:\n'+'\n'.join(contract_failures))
proof=json.loads((ROOT/'docs/ui-flow-results.json').read_text())
if not current('ui-flow-results.json',proof['application_version']) or proof['files_byte_exact']!=36:raise SystemExit('GUI flow proof mismatch')
for shot in proof['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('GUI screenshot proof mismatch')
shutil.copy2(ROOT/'docs/ui-flow-results.json',dist/'gui-flow-results.json')
original_gui=json.loads((ROOT/'docs/gui-original-results.json').read_text())
if not current('gui-original-results.json',original_gui['application_version']) or original_gui['original_files_byte_exact']!=36 or not original_gui['existing_pdf_preserved'] or not original_gui['source_unchanged']:raise SystemExit('GUI original-location proof mismatch')
shutil.copy2(ROOT/'docs/gui-original-results.json',dist/'gui-original-results.json')
shutil.copy2(ROOT/'docs/ui-demo-manifest.json',dist/'gui-demo-manifest.json')
jpeg_gui=json.loads((ROOT/'docs/gui-jpeg-results.json').read_text())
if not current('gui-jpeg-results.json',jpeg_gui['application_version']) or jpeg_gui['jpeg_files_byte_exact']!=33 or not jpeg_gui['source_unchanged']:raise SystemExit('JPEG GUI proof mismatch')
for shot in jpeg_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('JPEG screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-jpeg-results.json',dist/'gui-jpeg-results.json')
document_gui=json.loads((ROOT/'docs/gui-document-results.json').read_text())
if not current('gui-document-results.json',document_gui['application_version']) or document_gui['document_files_byte_exact']!=7 or not document_gui['source_unchanged']:raise SystemExit('Document GUI proof mismatch')
for shot in document_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('Document screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-document-results.json',dist/'gui-document-results.json')
imaging_gui=json.loads((ROOT/'docs/gui-imaging-results.json').read_text())
if not current('gui-imaging-results.json',imaging_gui['application_version']) or imaging_gui['source_bytes']!=512*1024*1024 or imaging_gui['copied_ranges']!=128 or imaging_gui['recovered_files_byte_exact']!=36 or imaging_gui['gui_map_pages_exercised']<2 or not all(imaging_gui[k] for k in ['image_source_byte_exact','source_unchanged','map_digest_unchanged_after_gui_resume','zip_crc_verified']):raise SystemExit('Imaging GUI proof mismatch')
for shot in imaging_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('Imaging screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-imaging-results.json',dist/'gui-imaging-results.json')
ntfs_gui=json.loads((ROOT/'docs/gui-ntfs-results.json').read_text())
if not current('gui-ntfs-results.json',ntfs_gui['application_version']) or ntfs_gui['ntfs_streams_byte_exact']!=21 or ntfs_gui['alias_names_verified']!=2 or ntfs_gui['alias_payloads_recovered']!=1 or not ntfs_gui['source_unchanged']:raise SystemExit('NTFS GUI proof mismatch')
for shot in ntfs_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('NTFS screenshot proof mismatch')
shutil.copy2(ROOT/'docs/gui-ntfs-results.json',dist/'gui-ntfs-results.json')
discovery_gui=json.loads((ROOT/'docs/gui-discovery-results.json').read_text())
if not current('gui-discovery-results.json',discovery_gui['application_version']) or discovery_gui['lost_files_byte_exact']!=6 or discovery_gui['remnant_streams_byte_exact']!=14 or not all(discovery_gui[k] for k in ['source_unchanged','zip_crc_verified']):raise SystemExit('Discovery GUI proof mismatch')
for shot in discovery_gui['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('Discovery screenshot proof mismatch')
    shutil.copy2(ROOT/'docs'/shot['file'],dist/Path(shot['file']).name)
shutil.copy2(ROOT/'docs/gui-discovery-results.json',dist/'gui-discovery-results.json')
raid_gui=json.loads((ROOT/'docs/gui-raid-results.json').read_text())
assert current('gui-raid-results.json',raid_gui['application_version']) and raid_gui['status']=='passed' and raid_gui['files_byte_exact']==36 and raid_gui['file_read_provenance_verified'] and raid_gui['source_unchanged']
for shot in raid_gui['screenshots']:
    assert hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()==shot['sha256']
    shutil.copy2(ROOT/'docs'/shot['file'],dist/Path(shot['file']).name)
shutil.copy2(ROOT/'docs/gui-raid-results.json',dist/'gui-raid-results.json')
indexed_gui=json.loads((ROOT/'docs/gui-index-results.json').read_text())
assert current('gui-index-results.json',indexed_gui['version']) and indexed_gui['status']=='passed' and indexed_gui['candidates']==1256 and indexed_gui['files_byte_exact']==4
assert 0<indexed_gui['gui_paused_candidates']<indexed_gui['gui_resumed_candidates']==1256 and indexed_gui['source_bytes_unchanged']
assert indexed_gui['selected_pages']==[1,2,3] and indexed_gui['selected_bytes']==185
for shot in indexed_gui['screenshots']:
    assert hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()==shot['sha256']
    shutil.copy2(ROOT/'docs'/shot['file'],dist/Path(shot['file']).name)
shutil.copy2(ROOT/'docs/gui-index-results.json',dist/'gui-index-results.json')
large_gui=json.loads((ROOT/'docs/gui-index-large-results.json').read_text())
assert current('gui-index-large-results.json',large_gui['application_version']) and large_gui['status']=='passed' and large_gui['candidates']==10000000
assert large_gui['files_byte_exact']==3 and large_gui['bytes_exact']==138 and large_gui['query_cancelled_by_keyboard'] and large_gui['archive_crc_valid']
for shot in large_gui['screenshots']:
    assert hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()==shot['sha256']
    shutil.copy2(ROOT/'docs'/shot['file'],dist/Path(shot['file']).name)
shutil.copy2(ROOT/'docs/gui-index-large-results.json',dist/'gui-index-large-results.json')
benchmark=json.loads((ROOT/'docs/index-benchmark-results.json').read_text())
assert current('index-benchmark-results.json',benchmark['version']) and benchmark['status']=='passed' and benchmark['functional_checks']['queries']==220
assert benchmark['final_queries']['status']=='passed' and benchmark['final_queries']['candidates']==10000000
assert all(not query['failures'] for query in benchmark['final_queries']['queries'].values())
shutil.copy2(ROOT/'docs/index-benchmark-results.json',dist/'index-benchmark-results.json')
for name in ['gui-raw-export-results.json','gui-preview-results.json','gui-allocation-results.json','gui-i18n-results.json','gui-job-results.json','gui-keyboard-results.json','gui-naming-results.json','gui-source-results.json']:
    proof=json.loads((ROOT/'docs'/name).read_text(encoding='utf-8'))
    if proof.get('application_version',proof.get('version'))!=VERSION or proof['status']!='passed':raise SystemExit('alpha.16 GUI proof mismatch: '+name)
    unchanged=proof.get('source_unchanged',proof.get('acquisition',{}).get('source_unchanged'))
    if not unchanged:raise SystemExit('alpha.16 GUI proof without unchanged source: '+name)
    for shot in proof['screenshots']:
        if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('alpha.16 screenshot mismatch: '+shot['file'])
        shutil.copy2(ROOT/'docs'/shot['file'],dist/Path(shot['file']).name)
    shutil.copy2(ROOT/'docs'/name,dist/name)
shutil.copy2(ROOT/'docs/gui-evidence-carryover.json',dist/'gui-evidence-carryover.json')
html=(ROOT/'docs/intelligence.html').read_text(encoding='utf-8')
def embedded(match):
    path=ROOT/'docs'/match.group(1)
    return 'src="data:image/png;base64,'+base64.b64encode(path.read_bytes()).decode('ascii')+'"'
html=re.sub(r'src="(images/[^"<>]+\.png)"',embedded,html)
(dist/'intelligence.html').write_text(html,encoding='utf-8')
for name in ['details-view.png','settings-view.png','icons-view.png','technical-ui-v15.png','technical-ui-v16.png','ntfs-extents.png','ntfs-extents-restored.png','ntfs-aliases.png','ntfs-aliases-restored.png','imaging-settings.png','imaging-progress.png','read-map.png','jpeg-carving.png','jpeg-restored.png','document-carving.png','document-restored.png','small-window.png','disk-selection.png','original-location.png','original-restored.png']:
    shutil.copy2(ROOT/'docs/images'/name,dist/name)
assets=sorted(path for path in dist.iterdir() if path.is_file() and path.name!='SHA256SUMS.txt')
(dist/'SHA256SUMS.txt').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in assets),encoding='ascii')
print(f'All four native packages verified; {len(assets)} release assets checksummed.')
