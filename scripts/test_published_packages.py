"""Test exact public portable CLI bytes, including macOS after ad-hoc signing."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tarfile
import time
import urllib.error
import urllib.request
import zipfile
from e2e_file_structures import verify as file_structures
from e2e_virtual_images import verify as virtual_images
from e2e_qcow2 import verify as qcow2
from e2e_apng import verify as animations
from e2e_icons import verify as icons
from e2e_rtf import verify as rtf_documents
from e2e_open_documents import verify as open_documents
from e2e_json import verify as json_documents
from e2e_udif import verify as udif_images
from e2e_compressed_raw import verify as compressed_disks
from e2e_udif_codecs import verify as modern_udif_codecs
from e2e_known_extents import verify as known_extents
from e2e_vhd import verify as classic_vhd
from e2e_aiff import verify as aiff_audio
from e2e_fonts import verify as font_files
from e2e_deleted_audio_fonts import verify as deleted_audio_fonts
from e2e_fingerprinted import verify as unordered_fingerprints, native as deleted_fingerprints
from e2e_aac import verify as aac_recordings
from e2e_tar import verify as tar_archives
from e2e_btrfs_snapshots import verify as btrfs_inline
from e2e_btrfs_extents import verify as btrfs_extents
from e2e_btrfs_layouts import verify as btrfs_layouts
from e2e_netpbm import verify as netpbm_images
from e2e_sevenz import verify as sevenz_archives
from e2e_cab import verify as cabinet_archives
from e2e_android_sparse import portable as android_sparse, native as aosp_deletions
from e2e_ddrescue import portable as ddrescue_map, native as gnu_ddrescue
from e2e_split_raw import portable as split_raw, native as gnu_split_raw, large as large_split_raw
from e2e_strided_files import portable as strided_files, native as deleted_strided_files
from e2e_whole_hash import portable as whole_hash_files, native as deleted_whole_hash_files
from e2e_vmdk import verify as native_vmdk
from e2e_vmdk_split import verify as split_vmdk
from e2e_vmdk_boundary import verify as vmdk_boundary
from e2e_vhdx_chunk import verify as vhdx_chunk
from e2e_vhdx_4kn import verify as native_4kn
from e2e_vhdx import verify as native_vhdx
from e2e_html import verify as html_documents
from e2e_ext4_journal import verify as ext4_journal
from e2e_ext4_journal_extents import verify as ext4_external_extents
from e2e_btrfs_previous import verify as btrfs_previous
from e2e_btrfs_checksums import verify as btrfs_checksums
from e2e_optical import verify as optical_tracks
from e2e_iso_sessions import verify as iso_history
from e2e_apfs_snapshots import verify as apfs_snapshot_files
from e2e_xml import verify as xml_documents
from e2e_mp3 import verify as mp3_audio
from e2e_flac import verify as flac_audio
from e2e_tiff import verify as tiff_images
from e2e_archive_streams import verify as archive_streams
from e2e_frame_archives import verify as frame_archives, golden_frames
from e2e_apple_partitions import verify as apple_partitions
from e2e_forensic_images import verify as forensic_images
from e2e_release_fixes import main as release_fixes
from e2e_acquisition import main as acquisition
from e2e_cp932 import main as cp932
from e2e_exfat_damage import main as exfat_damage
from e2e_known_hashes import main as known_hashes
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
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 4, 0):
        result['file_structure_verification'] = file_structures(binary)
        with tempfile.TemporaryDirectory() as tmp:
            result['virtual_image_verification'] = virtual_images(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 5, 0):
        for name, test in [('qcow2', qcow2), ('apng', animations), ('icons', icons)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[name + '_verification'] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 6, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['archive_stream_verification'] = archive_streams(binary, Path(tmp))
        with tempfile.TemporaryDirectory() as tmp:
            result['frame_archive_verification'] = frame_archives(binary, Path(tmp), golden_frames())
        with tempfile.TemporaryDirectory() as tmp:
            files = apple_partitions(binary, Path(tmp))
            result['apple_partition_files'] = {e['extension']: e['sha256'] for e in files}
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['forensic_images'] = forensic_images(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 7, 0):
        types = [('rtf', rtf_documents), ('xml', xml_documents), ('mp3', mp3_audio),
                 ('flac', flac_audio), ('tif', tiff_images)]
        result['media_document_verification'] = {}
        for family, test in types:
            with tempfile.TemporaryDirectory() as tmp:
                result['media_document_verification'][family] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 8, 0):
        for key, test in [('open_documents', open_documents), ('json_documents', json_documents), ('udif_images', udif_images)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 9, 0):
        for key, test in [('compressed_disks', compressed_disks), ('modern_udif', modern_udif_codecs), ('known_extents', known_extents)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 10, 0):
        for key, test in [('classic_vhd', classic_vhd), ('aiff_audio', aiff_audio), ('font_files', font_files)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['qemu_vhd'] = classic_vhd(binary, Path(tmp), native=True)
            with tempfile.TemporaryDirectory() as tmp:
                result['actual_deleted_audio_fonts'] = deleted_audio_fonts(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 11, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['unordered_fingerprints'] = unordered_fingerprints(binary, Path(tmp))
        if a.platform == 'linux':
            for key, test in [('fragmented_fat_files', deleted_fingerprints), ('apfs_snapshot_files', apfs_snapshot_files)]:
                with tempfile.TemporaryDirectory() as tmp:
                    result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 12, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['cdrom_tracks'] = optical_tracks(binary, Path(tmp), independent=a.platform == 'linux')
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['historical_iso_files'] = iso_history(binary, Path(tmp))
    disks(binary,(a.output/'native-disk-proof').resolve(),True)
    restore_paths(binary,(a.output/'restore-paths.json').resolve())
    restore_native(binary,(a.output/'restore-native.json').resolve())
    result['ntfs_extension_verification']=ntfs_extents(binary,(a.output/'ntfs-results.json').resolve())
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 25, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['ddrescue_map'] = ddrescue_map(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['gnu_ddrescue'] = gnu_ddrescue(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 24, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['split_raw'] = split_raw(binary, Path(tmp))
        if a.platform == 'linux':
            for key,test in [('gnu_split_raw',gnu_split_raw),('large_split_raw',large_split_raw)]:
                with tempfile.TemporaryDirectory() as tmp:
                    result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 23, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['strided_files'] = strided_files(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['deleted_strided_files'] = deleted_strided_files(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 22, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['android_sparse'] = android_sparse(binary,Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['android_deletions'] = aosp_deletions(binary,Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 21, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['whole_file_hashes'] = whole_hash_files(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['deleted_whole_file_hashes'] = deleted_whole_hash_files(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 20, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['sevenz_archives'] = sevenz_archives(binary, Path(tmp))
        with tempfile.TemporaryDirectory() as tmp:
            result['cab_archives'] = cabinet_archives(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['sevenz_deleted'] = sevenz_archives(binary, Path(tmp), native=True)
            with tempfile.TemporaryDirectory() as tmp:
                result['cab_deleted'] = cabinet_archives(binary, Path(tmp), native=True)
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 19, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['standalone_vmdk'] = native_vmdk(binary, Path(tmp), independent=a.platform == 'linux')
        with tempfile.TemporaryDirectory() as tmp:
            result['split_vmdk'] = split_vmdk(binary, Path(tmp), independent=a.platform == 'linux')
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['vmdk_boundary'] = vmdk_boundary(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 18, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['windows_vhdx'] = native_vhdx(binary, Path(tmp))
        with tempfile.TemporaryDirectory() as tmp:
            result['windows_4kn'] = native_4kn(binary, Path(tmp), independent=a.platform == 'linux')
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['qemu_vhdx'] = native_vhdx(binary, Path(tmp), native=True)
            with tempfile.TemporaryDirectory() as tmp:
                result['vhdx_chunk'] = vhdx_chunk(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 17, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['html_documents'] = html_documents(binary, Path(tmp))
        with tempfile.TemporaryDirectory() as tmp:
            result['netpbm_images'] = netpbm_images(binary, Path(tmp))
        if a.platform == 'linux':
            with tempfile.TemporaryDirectory() as tmp:
                result['html_deleted'] = html_documents(binary, Path(tmp), native=True)
            with tempfile.TemporaryDirectory() as tmp:
                result['netpbm_deleted'] = netpbm_images(binary, Path(tmp), native=True)
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 16, 0):
        with tempfile.TemporaryDirectory() as tmp:
            result['ext4_journal'] = ext4_journal(binary, Path(tmp), independent=a.platform == 'linux')
        with tempfile.TemporaryDirectory() as tmp:
            result['ext4_journal_extent_trees'] = ext4_external_extents(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 15, 0):
        for key, test in [('btrfs_previous', btrfs_previous), ('btrfs_checksums', btrfs_checksums)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 13, 0):
        for key, test in [('btrfs_inline', btrfs_inline), ('btrfs_extents', btrfs_extents), ('btrfs_layouts', btrfs_layouts)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
    if tuple(map(int, version.split('-')[0].split('.'))) >= (1, 14, 0):
        for key, test in [('aac_recordings', aac_recordings), ('tar_archives', tar_archives)]:
            with tempfile.TemporaryDirectory() as tmp:
                result[key] = test(binary, Path(tmp))
            if a.platform == 'linux':
                with tempfile.TemporaryDirectory() as tmp:
                    result[key + '_deleted'] = test(binary, Path(tmp), native=True)
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
    result['known_hashes_verification']=known_hashes(binary,a.output/'known-hashes-results.json')
    result['exfat_damage_verification']=exfat_damage(binary,a.output/'exfat-damage-results.json')
    result['cp932_verification']=cp932(binary,a.output/'cp932-results.json')
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
