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
    checksum_file=dist/f'SHA256SUMS-{platform}-{arch}.txt'
    for line in checksum_file.read_text().splitlines():
        expected,name=line.split('  ',1)
        if Path(name).name!=name or hashlib.sha256((dist/name).read_bytes()).hexdigest()!=expected:raise SystemExit('Native checksum mismatch: '+name)
proof=json.loads((ROOT/'docs/ui-flow-results.json').read_text())
if proof['application_version']!=VERSION or proof['files_byte_exact']!=36:raise SystemExit('GUI flow proof mismatch')
for shot in proof['screenshots']:
    if hashlib.sha256((ROOT/'docs'/shot['file']).read_bytes()).hexdigest()!=shot['sha256']:raise SystemExit('GUI screenshot proof mismatch')
shutil.copy2(ROOT/'docs/ui-flow-results.json',dist/'gui-flow-results.json')
shutil.copy2(ROOT/'docs/ui-demo-manifest.json',dist/'gui-demo-manifest.json')
html=(ROOT/'docs/intelligence.html').read_text(encoding='utf-8')
def embedded(match):
    path=ROOT/'docs'/match.group(1)
    return 'src="data:image/png;base64,'+base64.b64encode(path.read_bytes()).decode('ascii')+'"'
html=re.sub(r'src="(images/[^"<>]+\.png)"',embedded,html)
(dist/'intelligence.html').write_text(html,encoding='utf-8')
for name in ['details-view.png','settings-view.png','icons-view.png','technical-ui-v8.png','disk-selection.png','original-location.png','original-restored.png']:
    shutil.copy2(ROOT/'docs/images'/name,dist/name)
assets=sorted(path for path in dist.iterdir() if path.is_file() and path.name!='SHA256SUMS.txt')
(dist/'SHA256SUMS.txt').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in assets),encoding='ascii')
print(f'All four native packages verified; {len(assets)} release assets checksummed.')
