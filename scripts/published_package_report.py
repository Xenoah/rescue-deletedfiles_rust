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
    (a.output/name).write_bytes(path.read_bytes());runs.append(run)
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
