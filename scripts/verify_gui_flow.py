"""Verify artifacts from a real GUI run; this script does not operate the GUI."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import struct
import tomllib
import zipfile
from e2e import check

p=argparse.ArgumentParser();p.add_argument('--truth',type=Path,required=True);p.add_argument('--source',type=Path,required=True);p.add_argument('--work',type=Path,required=True);p.add_argument('--archive',type=Path,required=True);p.add_argument('--gui',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
root=Path(__file__).resolve().parent.parent
sha=lambda file:hashlib.sha256(file.read_bytes()).hexdigest()
truth=json.loads(a.truth.read_text(encoding='utf-8'))
check(sha(a.source)==truth['image_sha256'],'GUI modified original image')
files=[]
with zipfile.ZipFile(a.archive) as z:
    check(z.testzip() is None,'GUI ZIP CRC failure');report=json.loads(z.read('rescue-report.json'))
    session=json.loads((a.work/(report['session_id']+'.rescue-session.json')).read_text(encoding='utf-8'))
    check(set(truth['files'])==set(r['candidate_id'] for r in report['results']),'GUI selection differs')
    check(len(z.namelist())==len(truth['files'])+1,'extra or missing ZIP members')
    for c in session['candidates']:
        check(c['created']['value']==truth['files'][c['id']]['created'] and c['modified']['value']==truth['files'][c['id']]['modified'],'GUI scan date differs from fixture')
    for result in report['results']:
        key=result['candidate_id'];data=z.read(result['archive_entry']);digest=hashlib.sha256(data).hexdigest()
        check(result['status']=='completed' and len(data)==truth['files'][key]['bytes'] and digest==truth['files'][key]['sha256']==result['sha256'],'GUI saved incorrect bytes')
        files.append({'candidate_id':key,'archive_entry':result['archive_entry'],'bytes':len(data),'sha256':digest,'byte_exact':True})
images=[]
for name in ['disk-selection.png','settings-view.png','details-view.png','icons-view.png']:
    path=root/'docs/images'/name;data=path.read_bytes();check(data[:8]==b'\x89PNG\r\n\x1a\n','not a screenshot PNG');width,height=struct.unpack_from('>II',data,16);check((width,height)==(1260,820),'unexpected GUI capture dimensions')
    images.append({'file':'images/'+name,'width':width,'height':height,'sha256':sha(path)})
output={'schema_version':1,'time_utc':datetime.now(timezone.utc).isoformat(),'platform':platform.platform(),'application_version':tomllib.loads((root/'Cargo.toml').read_text())['workspace']['package']['version'],'scope':'GUI settings/scan/selection/ZIP recovery on Linux Xvfb; synthetic NTFS dates/folders; real corpus bytes','gui_binary_sha256':sha(a.gui),'gui_build':'Linux debug, separate from release native package startup checks','source_sha256_before_and_after':truth['image_sha256'],'archive_sha256':sha(a.archive),'files_byte_exact':len(files),'recorded_dates_matched':len(session['candidates']),'windows_manual_gui_test':False,'macos_manual_gui_test':False,'screenshots':images,'files':files}
a.output.write_text(json.dumps(output,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps({'status':'passed','GUI_ZIP_byte_exact':len(files),'date_records_matched':len(session['candidates']),'screenshot_hashes':len(images)}))
