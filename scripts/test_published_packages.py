"""Test exact public portable CLI bytes, including macOS after ad-hoc signing."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import time
import urllib.error
import urllib.request
import zipfile
from e2e_release_fixes import main as release_fixes
from e2e_acquisition import main as acquisition
from e2e_reports import main as reports
from e2e_classic_fat import main as classic_fat
from e2e_estimated_fat import main as estimated_fat
from independent_classic_fat import main as independent_classic_fat
from e2e_formats import main as formats
from e2e_jpeg import main as jpeg
from e2e_documents import main as documents
from e2e_ntfs_extents import main as ntfs_extents
from e2e_discovery import main as discovery
from e2e_ntfs_remnants import main as remnants
from e2e_quickformat import main as quickformat
from e2e_raid import main as raid
from e2e_raid_native import main as raid_native
from e2e_ntfs_native import main as ntfs_native
from e2e_imaging import main as imaging
from e2e_imaging_native import main as imaging_native
from e2e_features import main as features
from e2e_disks import main as disks
from e2e_restore import main as restore_paths
from e2e_restore_native import main as restore_native
from e2e_index import main as index
from e2e_index_carve import main as index_carve
from e2e_index_fat import main as index_fat
from e2e_index_remnants import main as index_remnants
from e2e_index_late import main as index_late
from e2e_index_discovery import main as index_discovery
from e2e_preview import main as preview
from e2e_allocation_scope import main as allocation_scope
from e2e_job_contract import main as job_contract
from e2e_naming import main as naming
from e2e_source_contract import main as source_contract
from e2e_export_raw import main as export_raw
from e2e_damaged_fat import main as damaged_fat
from verify_package import architecture


def recorded_suite(binary, suite, report):
    # Some suites write their proof to disk and deliberately return None.
    # The JSON record is the shared contract used by native release CI too.
    suite(binary, report)
    proof = json.loads(report.read_text(encoding='utf-8'))
    if not isinstance(proof, dict) or proof.get('status') != 'passed':
        raise RuntimeError('suite did not record a successful proof: ' + report.name)
    return proof


def main():
    p=argparse.ArgumentParser();p.add_argument('--tag',required=True);p.add_argument('--platform',required=True,choices=['windows','linux','macos']);p.add_argument('--arch',required=True,choices=['x64','arm64']);p.add_argument('--output',type=Path,required=True);p.add_argument('--independent',action='store_true');p.add_argument('--native-proof-dir',type=Path);p.add_argument('--defer-windows-gui',action='store_true',help='Run the unchanged Windows GUI suites as separate CI steps, then collect their proofs');a=p.parse_args()
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-alpha\.\d+)?',a.tag):raise ValueError('invalid release tag')
    base=f'https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/{a.tag}/'
    def download(name,limit):
        for attempt in range(4):
            try:
                request=urllib.request.Request(base+name+f'?download_attempt={attempt}',headers={'Cache-Control':'no-cache'})
                with urllib.request.urlopen(request,timeout=60) as response:data=response.read(limit+1)
                if len(data)>limit:raise RuntimeError('release download too large')
                return data
            except urllib.error.HTTPError as error:
                if error.code not in [408,429,500,502,503,504] or attempt==3:raise
            except (urllib.error.URLError,TimeoutError):
                if attempt==3:raise
            time.sleep(2**attempt)
    manifest={line.split('  ',1)[1]:line.split('  ',1)[0] for line in download('SHA256SUMS.txt',128*1024).decode('ascii').splitlines()}
    def verified(name,limit):
        data=download(name,limit)
        if hashlib.sha256(data).hexdigest()!=manifest[name]:raise RuntimeError('published checksum mismatch: '+name)
        return data
    from release_assets import package_names
    if set(manifest)!=set(package_names(a.tag.removeprefix('v'))):raise RuntimeError('Public checksum manifest must contain exactly eight distributions')
    version=a.tag.removeprefix('v');stem=f'rescue-{version}-{a.platform}-{a.arch}'
    name=stem+('-portable.tar.gz' if a.platform=='linux' else '-portable.zip')
    archive=verified(name,192*1024*1024)
    if a.native_proof_dir:
        proof=json.loads((a.native_proof_dir/f'verification-{a.platform}-{a.arch}.json').read_text(encoding='utf-8'))
        native=json.loads((a.native_proof_dir/f'package-{a.platform}-{a.arch}.json').read_text(encoding='utf-8'))
        expected=next(item['sha256'] for item in native['assets'] if item['file']==name)
        if native['version']!=version or hashlib.sha256(archive).hexdigest()!=expected:
            raise RuntimeError('Public portable differs from the release workflow artifact')
    else:
        proof=json.loads(verified(f'verification-{a.platform}-{a.arch}.json',128*1024))
    if proof['status']!='passed' or proof['version']!=version:raise RuntimeError('package verification mismatch')
    if a.platform=='linux':
        with tarfile.open(fileobj=io.BytesIO(archive),mode='r:gz') as tar:data=tar.extractfile(stem+'/rescue').read()
    else:
        with zipfile.ZipFile(io.BytesIO(archive)) as z:
            if z.testzip() is not None:raise RuntimeError('portable ZIP CRC mismatch')
            data=z.read('rescue.exe' if a.platform=='windows' else 'Rescue.app/Contents/MacOS/rescue')
            gui_data=z.read('rescue-gui.exe') if a.platform=='windows' else None
    digest=hashlib.sha256(data).hexdigest()
    if digest!=proof['portable_cli_sha256']:raise RuntimeError('CLI differs from verified packaged executable')
    a.output.mkdir(parents=True,exist_ok=True);binary=(a.output/('published-rescue.exe' if a.platform=='windows' else 'published-rescue')).resolve();binary.write_bytes(data);binary.chmod(0o755)
    architecture(binary,a.platform,a.arch)
    version_output=subprocess.check_output([str(binary),'--version'],text=True,encoding='utf-8').strip()
    if version_output!='rescue '+version:raise RuntimeError('released CLI version mismatch')
    features(binary)
    result=formats(binary,Path(__file__).resolve().parent.parent/'tests/corpus',a.output,a.independent)
    disks(binary,(a.output/'native-disk-proof').resolve(),True)
    restore_paths(binary,(a.output/'restore-paths.json').resolve())
    restore_native(binary,(a.output/'restore-native.json').resolve())
    result['ntfs_extension_verification']=ntfs_extents(binary,(a.output/'ntfs-results.json').resolve())
    result['raid_verification']=raid(binary,(a.output/'raid-results.json').resolve())
    if a.platform=='linux':result['raid_native_verification']=raid_native(binary,(a.output/'raid-native-results.json').resolve())
    result['discovery_verification']=discovery(binary,(a.output/'discovery-results.json').resolve())
    result['remnant_verification']=remnants(binary,(a.output/'remnant-results.json').resolve())
    if a.platform=='linux':result['quickformat_verification']=quickformat(binary,(a.output/'quickformat-results.json').resolve())
    if a.platform=='windows':result['ntfs_native_verification']=ntfs_native(binary,(a.output/'ntfs-native.json').resolve())
    result['jpeg_verification']=jpeg(binary,(a.output/'jpeg-results.json').resolve())
    result['document_verification']=documents(binary,(a.output/'document-results.json').resolve())
    result['imaging_verification']=imaging(binary,(a.output/'imaging-results.json').resolve())
    result['imaging_native_verification']=imaging_native(binary,(a.output/'imaging-native-results.json').resolve())
    for suite_name,suite in [('index',index),('index_carve',index_carve),('index_fat',index_fat),
                       ('index_remnants',index_remnants),('index_late',index_late),('index_discovery',index_discovery),
                       ('preview',preview),('allocation_scope',allocation_scope),('job_contract',job_contract),
                       ('naming',naming),('source_contract',source_contract),('export_raw',export_raw),('damaged_fat',damaged_fat)]:
        result[suite_name+'_verification']=recorded_suite(binary,suite,(a.output/(suite_name+'-results.json')).resolve())
    result['acquisition_verification']=acquisition(binary,a.output/'acquisition-results.json')
    result['release_fixes_verification']=release_fixes(binary,a.output/'release-fixes-results.json')
    result['reports_verification']=reports(binary,a.output/'reports-results.json')
    result['classic_fat_verification']=classic_fat(binary,a.output/'classic-fat-results.json')
    result['estimated_fat_verification']=estimated_fat(binary,a.output/'estimated-fat-results.json')
    if a.platform=='linux':result['independent_classic_fat_verification']=independent_classic_fat(binary,a.output/'independent-classic-fat-results.json')
    result['original_location_verification']=json.loads((a.output/'restore-native.json').read_text(encoding='utf-8'))
    result['original_path_cases']=json.loads((a.output/'restore-paths.json').read_text(encoding='utf-8'))
    result['disk_verification']=json.loads((a.output/'native-disk-proof/results.json').read_text(encoding='utf-8'))
    if a.platform=='windows':
        from e2e_gui_windows import main as gui_test
        gui=(a.output/'rescue-gui.exe').resolve();gui.write_bytes(gui_data)
        if hashlib.sha256(gui_data).hexdigest()!=proof['portable_gui_sha256']:raise RuntimeError('GUI differs from verified portable')
        (a.output/'rescue.exe').write_bytes(data)  # exact companion for GUI helpers
        result['gui_verification']={}
        if not a.defer_windows_gui:
            for kind,keyboard,ime in [('limited',False,False),('keyboard',True,False),('ime',True,True)]:
                path=(a.output/f'gui-{kind}.json').resolve()
                gui_test(gui,path,limited=True,desktop=True,keyboard=keyboard,ime=ime)
                result['gui_verification'][kind]=json.loads(path.read_text(encoding='utf-8'))
        result['public_gui_sha256']=hashlib.sha256(gui_data).hexdigest()
    result['published_release']={'tag':a.tag,'platform':a.platform,'arch':a.arch,'portable_sha256':hashlib.sha256(archive).hexdigest(),'executable_sha256':digest,'download_url':base+name,'tested_after_packaging_and_signing':True,'package_feature_suite_checks':60}
    result['ci_provenance']={'run_id':os.environ.get('GITHUB_RUN_ID'),'attempt':os.environ.get('GITHUB_RUN_ATTEMPT'),'commit':os.environ.get('GITHUB_SHA')}
    (a.output/'results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'status':'passed','published_release':a.tag,'platform':a.platform,'arch':a.arch,'binary_sha256':digest,'signed_or_packaged_bytes_tested':True}))

if __name__=='__main__':main()
