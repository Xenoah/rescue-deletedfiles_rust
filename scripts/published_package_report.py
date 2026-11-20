"""Summarize executed tests of public packaged CLI and required Windows GUI bytes."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
from ime_evidence import require_ime_cleanup, require_repeated_ime
p=argparse.ArgumentParser();p.add_argument('--input-dir',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--tag',required=True);p.add_argument('--body',type=Path,required=True);p.add_argument('--updated-body',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
runs=[];lines=[f'# {a.tag}: actual public package verification','','All four native platforms downloaded and checksum-verified the published portable, then executed its exact CLI bytes after packaging and macOS ad-hoc signing.','','| Platform | Metadata byte-exact | Native disk exact | All observations | CLI SHA-256 |','| --- | --- | --- | --- | --- |']
for platform,arch in [('windows','x64'),('linux','x64'),('macos','x64'),('macos','arm64')]:
    name=f'published-format-{platform}-{arch}.json';path=a.input_dir/name;run=json.loads(path.read_text(encoding='utf-8'));pub=run['published_release'];assert pub['tag']==a.tag and pub['tested_after_packaging_and_signing'] and pub['executable_sha256']==run['binary_sha256']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 4, 0):
        expected_types = {'avi', 'mkv', 'webm', 'ogg', 'gz'}
        structures = run['file_structure_verification']
        assert expected_types <= structures['session'].keys() and structures['session'] == structures['index']
        expected_hashes = sorted(structures['session'][e] for e in expected_types)
        virtual = run['virtual_image_verification']
        assert virtual['automatic'] == virtual['explicit'] and virtual['automatic']['restored_sha256'] == expected_hashes
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 5, 0):
        qcow = run['qcow2_verification']
        assert qcow['v2'] == qcow['v3'] and qcow['v3']['restored_sha256'] == expected_hashes
        assert len(run['apng_verification']) >= 7 and len(run['icons_verification']) >= 7


    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 6, 0):
        assert len(run['archive_stream_verification']) == 11 and len(run['frame_archive_verification']) == 10
        assert set(run['apple_partition_files']) == {'png', 'webp', 'bz2', 'xml', 'txt', 'ico', 'zip'}
        if platform == 'linux':
            disk_images = run['forensic_images']
            assert set(disk_images) == {'vhdx-fixed', 'vhdx-dynamic', 'aff', 'single', 'multiple'}
            first = disk_images['single']
            assert first['segments'] == 1 and disk_images['multiple']['segments'] > 1
            assert len(first['deleted_files']) == 7
            for evidence in disk_images.values():
                assert evidence['logical_sha256'] == first['logical_sha256'] and evidence['deleted_files'] == first['deleted_files']

    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 7, 0):
        media_documents = run['media_document_verification']
        assert set(media_documents) == {'rtf', 'xml', 'mp3', 'flac', 'tif'}
        assert all(len(variants) >= 8 for variants in media_documents.values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 8, 0):
        assert len(run['open_documents']) == 21 and len(run['json_documents']) == 12
        assert set(run['udif_images']) == {'raw', 'zlib', 'bzip2', 'adc', 'mixed', 'data-fork-offset', 'libdmg-apm-fat32'}
        assert all(count == 7 for count in run['udif_images'].values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 9, 0):
        assert set(run['compressed_disks']) == {'gz', 'bz2', 'xz', 'zst', 'lz4'}
        assert all(n == 7 for n in run['compressed_disks'].values())
        assert len(run['modern_udif']) == 8 and all(n == 7 for n in run['modern_udif'].values())
        assert len(run['known_extents']) == 11
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 10, 0):
        assert run['classic_vhd'] == {'fixed': 7, 'dynamic': 7}
        assert len(run['aiff_audio']) == 10 and len(run['font_files']) == 10
        if platform == 'linux':
            assert run['qemu_vhd'] == {'qemu-fixed': 7, 'qemu-dynamic': 7}
            assert len(run['actual_deleted_audio_fonts']) == 10
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 11, 0):
        assert len(run['unordered_fingerprints']) == 10
        if platform == 'linux':
            assert len(run['fragmented_fat_files']) == 7
            history = run['apfs_snapshot_files']
            assert history['bare'] == history['offset-split'] and len(history['bare']) == 7
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 38, 0):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        reference = json.loads((docs/'openexr-results.json').read_text(encoding='utf-8'))
        assert run['openexr'] == reference['portable']
        if platform == 'linux':
            assert run['openexr_deleted'] == reference['native']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 37, 0):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        assert run['vmdk_cid'] == json.loads((docs/'vmdk-cid-results.json').read_text(encoding='utf-8'))
        if platform == 'linux':
            assert run['vmdk_cid_boundary'] == json.loads((docs/'vmdk-cid-boundary-results.json').read_text(encoding='utf-8'))
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 36, 0):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        assert run['vhdx_chain'] == json.loads((docs/'vhdx-backing-results.json').read_text(encoding='utf-8'))
        if platform == 'linux':
            assert run['vhdx_bitmap_boundary'] == json.loads((docs/'vhdx-bitmap-boundary-results.json').read_text(encoding='utf-8'))
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 35, 0):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        reference = json.loads((docs/'jpeg2000-results.json').read_text(encoding='utf-8'))
        assert run['jpeg2000'] == reference['portable']
        if platform == 'linux':
            assert run['jpeg2000_deleted'] == reference['native']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 34, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        assert run['vhd_chain'] == json.loads((docs/'vhd-backing-results.json').read_text())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 33, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        assert run['xfs_allocation'] == json.loads((docs/'xfs-allocation-results.json').read_text())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 32, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        reference = json.loads((docs/'qcow2-backing-results.json').read_text())
        assert run['qcow2_chain'] == reference['portable']
        if platform == 'linux':
            chains = run['qemu_chains']
            assert chains.keys() == reference['native'].keys()
            for name, sample in reference['native'].items():
                assert all(chains[name][k] == sample[k] for k in ('originals', 'guest_bytes', 'qemu_depths'))
            assert chains['windows-ntfs-zstd'] == reference['native']['windows-ntfs-zstd']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 31, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        reference = json.loads((docs/'photoshop-results.json').read_text(encoding='utf-8'))
        assert run['photoshop_documents'] == reference['portable']
        if platform == 'linux':
            assert run['photoshop_deleted'] == reference['native']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 30, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        assert run['ext4_allocation'] == json.loads((docs/'ext4-allocation-results.json').read_text())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 29, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        assert run['hfs_allocation'] == json.loads((docs/'hfs-allocation-results.json').read_text())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 28, 0):
        docs = Path(__file__).resolve().parents[1]/'docs'
        reference = json.loads((docs/'qcow-v1-results.json').read_text())
        assert run['qcow1'] == reference['portable']
        assert run['qcow1_geometry'] == json.loads((docs/'qcow-v1-geometry.json').read_text())
        if platform == 'linux':
            layouts = run['qemu_qcow1']
            assert layouts.keys() == reference['native'].keys()
            for label, result in layouts.items():
                assert result['originals'] == reference['native'][label]['originals']
                assert result['plain_clusters'] > 0 and result['unallocated_l2_entries'] > 0
                assert (result['deflate_clusters'] > 0) == label.endswith('-deflate')
            for fs in ('fat32','ext4','windows-ntfs'):
                assert layouts[fs+'-sparse']['disk_sha256'] == layouts[fs+'-deflate']['disk_sha256']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 27, 0):
        reference = json.loads((Path(__file__).resolve().parents[1]/'docs/heif-v1.27-results.json').read_text())
        assert run['heif_photos'] == reference['portable']
        if platform == 'linux':
            photos = run['heif_deleted']
            assert photos['originals'] == reference['portable']['originals']
            assert photos['filesystems'] == ['fat32','ext4']
            assert photos['metadata'] == reference['linux']['metadata']
            assert set(photos['decoded']) == set(photos['originals'])
            assert all(len(frames) == (2 if name == 'collection.heic' else 1)
                       for name, frames in photos['decoded'].items())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 26, 0):
        assert run['qcow2_subclusters'] == run['android_sparse']
        if platform == 'linux':
            images = run['qemu_subclusters']
            assert set(images) == {'fat32', 'ext4', 'windows-ntfs'}
            assert images['windows-ntfs']['originals'] == run['windows_vhdx']['fixed']
            assert all(images[f]['originals'] == run['qcow2_subclusters'] and images[f]['extended_l2']
                       for f in ('fat32', 'ext4'))
            assert images['fat32']['compression'] == images['windows-ntfs']['compression'] == 'zstd'
            assert all(i['compressed_clusters'] > 0 for i in images.values())
            shifted = run['shifted_ddrescue_partitions']
            reference = json.loads((Path(__file__).resolve().parents[1]/'docs/ddrescue-shifted-v1.25-results.json').read_text())
            assert shifted == reference
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 25, 0):
        assert run['ddrescue_map'] == run['android_sparse']
        if platform == 'linux':
            captures = run['gnu_ddrescue']
            assert set(captures) == {'fat32', 'ext4', 'windows-ntfs'}
            assert captures['windows-ntfs']['recovered'] == run['windows_vhdx']['fixed']
            for fs in ['fat32', 'ext4']:
                assert captures[fs]['recovered'] == run['ddrescue_map']
                fault = captures[fs]['with_read_error']
                assert fault['stale_wav_refused'] and fault['failed_sector'] != fault['untried_sector']
                assert fault['recovered'] == {k:v for k,v in run['ddrescue_map'].items() if k != 'sample.wav'}
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 24, 0):
        segments = run['split_raw']
        assert segments['originals'] == run['windows_vhdx']['fixed'] and segments['segments'] == 8
        if platform == 'linux':
            assert set(run['gnu_split_raw']) == {'fat32', 'ext4'}
            for disk in run['gnu_split_raw'].values():
                assert disk['originals'] == run['android_sparse'] and disk['crossing_segments'] == 8
            large = run['large_split_raw']
            assert large['logical_bytes'] > large['first_segment_bytes'] > 4 << 30
            assert large['originals'] == segments['originals']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 23, 0):
        assert run['strided_files'] == run['whole_file_hashes']
        if platform == 'linux':
            assert set(run['deleted_strided_files']) == {'12', '16', '32'}
            for files in run['deleted_strided_files'].values():
                assert {name: row['sha256'] for name,row in files.items()} == run['strided_files']
                assert all(row['runs'] == len(row['offsets']) >= 3 for row in files.values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 22, 0):
        truth=json.loads((Path(__file__).resolve().parents[1]/'tests/android-sparse/manifest.json').read_text())
        assert run['android_sparse'] == truth['originals'] and len(truth['originals']) == 7
        if platform == 'linux':
            captures = run['android_deletions']
            assert set(captures) == {'fat32','ext4','fat32-holes'}
            assert all(row['originals']==truth['originals'] for row in captures.values())
            assert captures['fat32-holes']['unrecorded_bytes'] > 0
            assert captures['fat32']['unrecorded_bytes'] == captures['ext4']['unrecorded_bytes'] == 0
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 21, 0):
        originals = run['whole_file_hashes']
        assert set(originals) == {'NOTE.TXT','DATA.CSV','PAGE.HTM','SONG.WAV','FILES.ZIP','VENDOR.BIN','DUP.DAT'}
        if platform == 'linux':
            layouts = run['deleted_whole_file_hashes']
            assert set(layouts) == {'12','16','32'}
            for files in layouts.values():
                assert {name: row['sha256'] for name,row in files.items()} == originals
                assert all(len(row['extents']) == 2 and row['bytes'] == sum(s['length'] for s in row['extents']) for row in files.values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 20, 0):
        assert len(run['sevenz_archives']) == len(run['cab_archives']) == 9
        if platform == 'linux':
            assert run['sevenz_deleted'] == run['sevenz_archives'] and run['cab_deleted'] == run['cab_archives']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 19, 0):
        vmdk = run['standalone_vmdk']
        assert set(vmdk) == {'sparse', 'zero-grain', 'stream', 'footer', 'footer-only', 'tail'} and all(rows == run['windows_vhdx']['fixed'] for rows in vmdk.values())
        split = run['split_vmdk']
        expected = {'flat', 'flat-split', 'sparse-split'}
        if platform == 'linux':
            expected.update(('monolithicFlat', 'twoGbMaxExtentFlat', 'twoGbMaxExtentSparse'))
            crossing = run['vmdk_boundary']
            assert set(crossing) == {'twoGbMaxExtentFlat','twoGbMaxExtentSparse'}
            assert crossing['twoGbMaxExtentFlat'] == crossing['twoGbMaxExtentSparse']
            for evidence in crossing.values():
                assert evidence['bytes'] > 2 << 30 and len(evidence['guest_sha256']) == 64
                assert evidence['before'] == 3 and evidence['after'] == 4
                assert evidence['originals'] == run['windows_vhdx']['fixed']
        assert set(split) == expected and all(rows == run['windows_vhdx']['fixed'] for rows in split.values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 18, 0):
        vhdx = run['windows_vhdx']
        assert vhdx['fixed'] == vhdx['expandable'] and len(vhdx['fixed']) == 7
        assert run['windows_4kn']['session'] == run['windows_4kn']['index'] == vhdx['fixed']
        if platform == 'linux':
            cross = run['vhdx_chunk']
            assert cross['before_boundary'] == 3 and cross['after_boundary'] == 4 and cross['bytes'] > (4 << 30)
            assert cross['originals'] == vhdx['fixed'] and len(cross['guest_sha256']) == 64
            assert set(run['qemu_vhdx']) == {'fixed', 'expandable', 'qemu-fixed', 'qemu-dynamic'}
            assert all(items == vhdx['fixed'] for items in run['qemu_vhdx'].values())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 17, 0):
        assert len(run['html_documents']) == 7 and len(run['netpbm_images']) == 9
        if platform == 'linux':
            assert run['html_deleted'] == run['html_documents']
            assert run['netpbm_deleted'] == run['netpbm_images']
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 16, 0):
        for key in ('ext4_journal','ext4_journal_extent_trees'):
            assert set(run[key]) == {'1024','4096'} and run[key]['1024'] == run[key]['4096'] and len(run[key]['1024']) == 7
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 15, 0):
        originals = run['btrfs_extents']['zstd']
        assert len(originals) == 7
        for key, modes in [('btrfs_previous', ('plain', 'zstd')), ('btrfs_checksums', ('xxhash', 'sha256', 'blake2'))]:
            assert set(run[key]) == set(modes) and all(run[key][mode] == originals for mode in modes)
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 14, 0):
        assert len(run['aac_recordings']) == 7 and len(run['tar_archives']) == 9
        if platform == 'linux':
            assert run['aac_recordings'] == run['aac_recordings_deleted']
            assert len(run['tar_archives_deleted']) == 7
            assert all(run['tar_archives'][n] == h for n,h in run['tar_archives_deleted'].items())
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 13, 0):
        for key, modes, count in [('btrfs_inline', ('plain', 'zlib'), 7), ('btrfs_extents', ('zlib', 'zstd'), 7), ('btrfs_layouts', ('lzo', 'zstd'), 13)]:
            assert set(run[key]) == set(modes)
            assert run[key][modes[0]] == run[key][modes[1]] and len(run[key][modes[0]]) == count
    if tuple(map(int, a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1, 12, 0):
        assert run['cdrom_tracks'] == {f'{mode}-{stride}': 7 for mode in ('mode1', 'xa-form1') for stride in (2352, 2448)}
        if platform == 'linux':
            assert len(run['historical_iso_files']) == 11
    assert run['formats']==35 and run['corpus_files']==36
    cases={'ntfs-contiguous','ntfs-fragmented','ntfs-compressed','ntfs-ads','fat32-contiguous','fat32-fragmented','exfat-contiguous','exfat-fragmented'};rows=[r for r in run['rows'] if r['case'] in cases];assert len(rows)==576 and all(r['byte_exact'] for r in rows)
    assert all(r['byte_exact'] for r in run['rows'] if r['expected']=='byte_exact')
    disk=run['disk_verification'];assert disk['native_virtual_disk'] and disk['byte_exact']==864
    original=run['original_location_verification'];assert original['status']=='passed' and original['actual_deletion'] and original['native_same_volume'] and original['cli_pdf_byte_exact'] and original['mcp_pdf_byte_exact'] and original['session_work_never_created']
    assert run['original_path_cases']['status']=='passed' and run['original_path_cases']['checks']>=58
    jpeg=run['jpeg_verification'];assert jpeg['status']=='passed' and jpeg['jpeg_candidates_exact']==33 and jpeg['malformed_cases']==33 and jpeg['cli_mcp_byte_exact'] and jpeg['source_unchanged']
    documents=run['document_verification'];assert documents['status']=='passed' and documents['valid_variants']>=23 and documents['rejected_corrupt_or_unsupported']>=30 and documents['binary_sha256']==run['binary_sha256']
    ntfs=run['ntfs_extension_verification'];assert ntfs['status']=='passed' and ntfs['checks']>=27 and ntfs['binary_sha256']==run['binary_sha256']
    raid=run['raid_verification'];assert raid['status']=='passed' and raid['checks']>=22 and raid['logical_arrays_byte_exact']==44 and raid['files_byte_exact']==73 and raid['binary_sha256']==run['binary_sha256']
    if run['published_release']['platform']=='linux':
        raid=run['raid_native_verification'];assert raid['status']=='passed' and raid['arrays_created']==5 and raid['logical_arrays_byte_exact']==12 and raid['files_byte_exact']==72 and raid['binary_sha256']==run['binary_sha256']
    for proof_key,minimum in [('discovery_verification',20),('remnant_verification',12)]:
        proof=run[proof_key];assert proof['status']=='passed' and proof['checks']>=minimum and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    if platform=='linux':
        proof=run['quickformat_verification'];assert proof['status']=='passed' and proof['actual_quickformat'] and not proof['metadata_edited'] and proof['cli_files_byte_exact']==100 and proof['mcp_files_byte_exact']==100 and proof['binary_sha256']==run['binary_sha256']
    if platform=='windows':
        native=run['ntfs_native_verification'];assert native['status']=='passed' and native['actual_deletion'] and native['cli_streams_byte_exact']==21 and native['mcp_streams_byte_exact']==21 and native['resident_stream_profile'] and len(native['nonresident_profiles'])==2 and all(p['policy']['restored'] and (p['erased_metadata_not_fabricated'] or p['retained_streams_byte_exact']==21) for p in native['nonresident_profiles']) and native['binary_sha256']==run['binary_sha256']
        if a.tag.startswith('v1.'):
            for kind in ['limited','keyboard','ime']:
                gui=run['gui_verification'][kind]
                assert gui['status']=='passed' and gui['source_unchanged'] and gui['files_byte_exact']==2 and gui['gui_binary_sha256']==run['public_gui_sha256']
                assert gui['uia']['gui_token']['elevated']==0 and not gui['uia']['gui_token']['administrator_enabled'] and gui['uia']['gui_token']['integrity']==8192
                if kind=='keyboard':assert gui['uia']['native_keyboard_input_tested']
                if kind=='ime':
                    ime=gui['uia']
                    if tuple(map(int,a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1,2,2):
                        require_ime_cleanup(gui,run['ime_profile_cleanup'])
                    else:assert ime['input_layout_restored']
                    assert ime['native_input_method_tested'] and not ime['native_ime']['unicode_injection_used'] and ime['native_ime']['committed_value']=='原文' and ime['native_ime']['cancel_trace'][-1]['value']=='' and ime['native_ime']['cancel_escape_presses']==2
                    if tuple(map(int,a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1,2,1):require_repeated_ime(gui, require_stages=tuple(map(int,a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1,2,2))
    image=run['imaging_verification'];assert image['status']=='passed' and image['checks']>=15 and image['binary_sha256']==run['binary_sha256']
    image_native=run['imaging_native_verification'];assert image_native['status']=='passed' and image_native['native_permission_denied'] and image_native['resume_after_capacity_failure_byte_exact'] and image_native['source_unchanged'] and image_native['binary_sha256']==run['binary_sha256']
    assert disk['native_access']['acquisition_byte_exact'] and disk['native_access']['acquired_files_byte_exact']==108
    for suite_name,minimum,files in [('preview',9 if platform=='windows' else 11,None),('allocation_scope',19,24),
                               ('job_contract',12,6),('naming',14,13),('source_contract',10,10),
                               ('export_raw',7,2),('damaged_fat',5,20)]:
        proof=run[suite_name+'_verification']
        assert proof['status']=='passed' and proof['checks']>=minimum and proof['source_unchanged']
        assert proof['version']=='rescue '+a.tag.removeprefix('v') and proof['binary_sha256']==run['binary_sha256']
        if files is not None:assert proof['files_byte_exact']==files
    for suite,checks,files in [('classic_fat',20,60),('estimated_fat',20,10),('reports',14,6),('release_fixes',9,9)]:
        proof=run[suite+'_verification'];assert proof['status']=='passed' and proof['checks']>=checks and proof['files_byte_exact']==files and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
        if suite=='estimated_fat':assert proof['known_incorrect_estimates_labelled_unverified']==1
    if a.tag.startswith('v1.2.'):
        proof=run['acquisition_verification'];assert proof['status']=='passed' and proof['checks']>=13 and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    if a.tag.startswith('v1.2.') and a.tag!='v1.2.0-alpha.1':
        proof=run['cp932_verification'];assert proof['status']=='passed' and proof['checks']>=34 and proof['files_byte_exact']==168 and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    if a.tag.startswith('v1.2.') and a.tag not in ['v1.2.0-alpha.1','v1.2.0-alpha.2']:
        proof=run['exfat_damage_verification'];assert proof['status']=='passed' and proof['checks']>=31 and proof['files_byte_exact']==72 and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    if a.tag.startswith('v1.2.') and a.tag not in ['v1.2.0-alpha.1','v1.2.0-alpha.2','v1.2.0-alpha.3']:
        proof=run['known_hashes_verification'];assert proof['status']=='passed' and proof['checks']>=18 and proof['files_byte_exact']==32 and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    if platform=='linux':
        proof=run['independent_classic_fat_verification'];assert proof['status']=='passed' and proof['files_byte_exact']==8 and proof['source_unchanged'] and proof['binary_sha256']==run['binary_sha256']
    lines.append(f"| {platform} {arch} | 576 / 576 | 864 / 864 | {len(run['rows'])} | `{run['binary_sha256']}` |")
    (a.output/path.name).write_bytes(path.read_bytes());runs.append(run)
lines+=['','NTFS: 27 extension/allocation/alias scenarios per platform. Windows additionally deleted an OS-created file with 20 resident ADS and two hard-link names on a new VHD; all 21 resident streams matched through CLI and MCP. Nonresident streams were checked separately with TRIM notifications enabled and disabled: erased descriptors must produce no successful empty files, while retained content must match the original.','Acquisition: CLI/MCP process-kill resume, torn journals, strict gap propagation, source identity and stable map pages; actual read-only native disks acquired byte-exact and 108 files restored. Each native OS also exercised a full owned virtual volume, resumed after releasing its filler file and rejected permission-denied output.','Document/media carving: 69 cases, 23 valid variants, 30 rejected malformed/unsupported conditions and 30 byte-exact CLI/MCP outputs on each exact public CLI.','JPEG carving: 32 synthetic JPEGs plus an embedded thumbnail (33 JPEG candidates) were byte-exact through CLI/MCP; 33 malformed structures/entropy cases were rejected. PNG co-scanning, ranges, aliases, ZIP, collisions and revalidation passed.', 'The 35 format families / 36 files also passed the 60-case ZIP/date/search/MCP feature suite on each public CLI.','Each exact public CLI additionally passed original-path/collision cases and two actual PDF deletions/restorations to the same newly created mounted virtual volume through CLI and MCP. Session work directories were never created on that source volume.','Observation totals include expected unsupported and data-loss cases; they are not general recovery rates.','Linux additionally tested independent filesystem tools and real mtools FAT32 deletion. Mac signing changes the CLI hash; the original format-results JSON records the pre-signing test, while these published-format JSON files record the actual signed portable bytes.','','Whole-disk access uses new read-only loop/VHD/hdiutil devices with synthetic deletion metadata; original-location tests use new writable virtual volumes and actual PDF deletion. These do not test real HDD/SSD recovery, TRIM, paid products, or manual Windows/macOS GUI operation.']
lines+=['','Issues 1-3 regression suite passed nine checks and nine exact raw outputs per public CLI: long ASCII/Unicode names, batch and map failure accounting, MCP, persisted FAT mirror provenance across a real process kill/resume, literal colons and explicit ADS type keys.']
lines+=['','HTML/CSV report export passed 14 checks per public CLI: exact recovery bytes, preservation of full source/results/uncertainty, escaped HTML, neutralized spreadsheet formulas, Unicode/newlines, bounds and no overwrite.']
lines+=['','FAT12/16 fixed-root, packed-table, partition and CLI/index/MCP recovery matched 60 files per platform. Explicit FAT estimation matched 10 known files and refused corrupt/allocated extents. One deliberately fragmented unknown-format file produced incorrect bytes and remained labelled unverified, never counted exact. Linux also recovered eight actual mtools deletions on FAT12/16.']
lines+=['','The seven alpha.16 suites also passed on each exact public CLI: isolated preview, allocation scope, job contract, naming, source identity, raw export and damaged FAT mirrors. Damaged FAT recovery matched 20 outputs; raw export matched two outputs and retained explicit unreadable-range evidence.']
if a.tag.startswith('v1.'):
    lines+=['','The exact public Windows GUI additionally passed UIA Invoke/Value, native SendInput keyboard and installed Microsoft Japanese IME flows under a verified non-elevated, medium-integrity token with administrator membership disabled. All three recovered the two selected Unicode-named files byte-exact, preserving the source. Native Roman-key IME conversion and cancellation passed, and the input layout was restored. Screen-reader product evaluation remains unmeasured.']
(a.output/'published-format-verification.json').write_text(json.dumps({'release_tag':a.tag,'scope':'actual public CLI bytes, after packaging/signing','runs':runs},ensure_ascii=False,indent=2)+'\n',encoding='utf-8');(a.output/'published-format-verification.md').write_text('\n'.join(lines)+'\n',encoding='utf-8')
expected={f'published-format-{platform}-{arch}.json' for platform,arch in [('windows','x64'),('linux','x64'),('macos','x64'),('macos','arm64')]} | {'published-format-verification.json','published-format-verification.md'}
files=sorted(a.output.glob('*'));assert {f.name for f in files}==expected, 'published evidence filenames differ';(a.output/'PUBLISHED-SHA256SUMS.txt').write_text(''.join(f'{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n' for f in files),encoding='ascii')
marker='<!-- published-package-verification -->';body=a.body.read_text(encoding='utf-8').split(marker)[0].rstrip();append=f'''\n\n{marker}\n## 公開パッケージそのものの追加検証\n\nWindows・Linux・macOS Intel/Apple Siliconで、公開ポータブルを再取得・チェックサム確認後、同梱CLIそのものを実行しました。macOSはアドホック署名後のファイルです。35形式・36ファイルの8条件×CLI/MCPは各環境576/576バイト一致。MBR/GPTディスクイメージと新規読み取り専用のloop/VHD/hdiutil仮想ディスクでも各環境864/864が一致。ZIP・日時・検索等の60項目も各環境で成功しました。\n\n文書・動画carvingの69ケース（正常23変種、破損/未対応30条件、30出力の全バイト一致）も各環境で通過。JPEG carvingの追加58項目も各環境で通過。32枚と埋込サムネイルの33候補をCLI／MCPでバイト照合し、33件の破損入力を拒否しました。\n\nイメージ取得では、強制終了後の再開・欠損範囲・読取マップのページ取得を検証。実仮想ディスクから全バイト一致で取得し、取得後の108ファイルを復元しました。試験専用ボリュームの容量不足・空き解放後の再開・アクセス拒否も各OSで確認しています。\n\nalpha.16の追加7群（隔離プレビュー・割当範囲・ジョブ契約・名前の履歴・ソース識別・バイナリ書き出し・破損FAT）も、各環境の公開CLIで通過しました。破損FATは20出力、バイナリ書き出しは2出力をバイト照合し、読取不能範囲の記録も確認しています。\n\n追加結果とチェックサムは、この検証ワークフローの **published-verification** アーティファクトに保存しています。リリース添付は配布物と共通チェックサムだけです。署名前のformat-results JSONと、署名・梱包後のこの検証記録を区別します。実媒体での一般的な復元率を示す結果ではありません。\n''';
if a.tag.startswith('v1.'):append+='\n公開Windows GUIそのものでも、非昇格・中整合性・管理者グループ無効を実プロセスで確認し、UIA操作・ネイティブキー入力・実Microsoft日本語IMEの3経路を実行し、日本語名2ファイルの選択ZIP復元と元媒体の不変を照合しました。IMEのローマ字変換・取消と入力設定の復元も確認しました。スクリーンリーダー製品の評価は未実施です。\n'
if tuple(map(int,a.tag.removeprefix('v').split('-')[0].split('.'))) >= (1,2,1):append+='\n公開Windows GUIの日本語IMEは同じ非昇格GUI内で取消・再変換を8回連続実行し、すべて成功しました。各回の実画面は、このワークフローの **public-windows-gui-screenshots** アーティファクトに保持しています。\n'
append+='\n[検証ワークフローと結果](https://github.com/'+os.environ.get('GITHUB_REPOSITORY','Xenoah/rescue-deletedfiles_rust')+'/actions/runs/'+os.environ.get('GITHUB_RUN_ID','')+')\n'
a.updated_body.write_text(body+append,encoding='utf-8');print('\n'.join(lines))
