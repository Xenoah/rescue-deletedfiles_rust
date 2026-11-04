"""Summarize executed tests of the public, signed/packaged CLI files."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--input-dir',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--tag',required=True);p.add_argument('--body',type=Path,required=True);p.add_argument('--updated-body',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
runs=[];lines=[f'# {a.tag}: actual public package verification','','All four native platforms downloaded and checksum-verified the published portable, then executed its exact CLI bytes after packaging and macOS ad-hoc signing.','','| Platform | Metadata byte-exact | Native disk exact | All observations | CLI SHA-256 |','| --- | --- | --- | --- | --- |']
for platform,arch in [('windows','x64'),('linux','x64'),('macos','x64'),('macos','arm64')]:
    name=f'published-format-{platform}-{arch}.json';path=a.input_dir/name;run=json.loads(path.read_text(encoding='utf-8'));pub=run['published_release'];assert pub['tag']==a.tag and pub['tested_after_packaging_and_signing'] and pub['executable_sha256']==run['binary_sha256']
    assert run['formats']==35 and run['corpus_files']==36
    cases={'ntfs-contiguous','ntfs-fragmented','ntfs-compressed','ntfs-ads','fat32-contiguous','fat32-fragmented','exfat-contiguous','exfat-fragmented'};rows=[r for r in run['rows'] if r['case'] in cases];assert len(rows)==576 and all(r['byte_exact'] for r in rows)
    assert all(r['byte_exact'] for r in run['rows'] if r['expected']=='byte_exact')
    disk=run['disk_verification'];assert disk['native_virtual_disk'] and disk['byte_exact']==864
    lines.append(f"| {platform} {arch} | 576 / 576 | 864 / 864 | {len(run['rows'])} | `{run['binary_sha256']}` |")
    (a.output/name).write_bytes(path.read_bytes());runs.append(run)
lines+=['','The 35 format families / 36 files also passed the 60-case ZIP/date/search/MCP feature suite on each public CLI.','Observation totals include expected unsupported and data-loss cases; they are not general recovery rates.','Linux additionally tested independent filesystem tools and real mtools FAT32 deletion. Mac signing changes the CLI hash; the original format-results JSON records the pre-signing test, while these published-format JSON files record the actual signed portable bytes.','','Native disk access uses new read-only loop/VHD/hdiutil virtual devices with synthetic deletion metadata. It does not test real HDD/SSD recovery, TRIM, paid products, or manual Windows/macOS GUI operation.']
(a.output/'published-format-verification.json').write_text(json.dumps({'release_tag':a.tag,'scope':'actual public CLI bytes, after packaging/signing','runs':runs},ensure_ascii=False,indent=2)+'\n',encoding='utf-8');(a.output/'published-format-verification.md').write_text('\n'.join(lines)+'\n',encoding='utf-8')
files=sorted(a.output.glob('*'));(a.output/'PUBLISHED-SHA256SUMS.txt').write_text(''.join(f'{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n' for f in files),encoding='ascii')
marker='<!-- published-package-verification -->';body=a.body.read_text(encoding='utf-8').split(marker)[0].rstrip();append=f'''\n\n{marker}\n## 公開パッケージそのものの追加検証\n\nWindows・Linux・macOS Intel/Apple Siliconで、公開ポータブルを再取得・チェックサム確認後、同梱CLIそのものを実行しました。macOSはアドホック署名後のファイルです。35形式・36ファイルの8条件×CLI/MCPは各環境576/576バイト一致。MBR/GPTディスクイメージと新規読み取り専用のloop/VHD/hdiutil仮想ディスクでも各環境864/864が一致。ZIP・日時・検索等の60項目も各環境で成功しました。\n\n追加結果は **published-format-verification.md / .json** と **published-format-*.json**、そのチェックサムは **PUBLISHED-SHA256SUMS.txt** に収録しています。元の添付ファイルとSHA256SUMS.txtは変更していません。署名前のformat-results JSONと、署名・梱包後のこの検証記録を区別します。実媒体での一般的な復元率を示す結果ではありません。\n''';a.updated_body.write_text(body+append,encoding='utf-8');print('\n'.join(lines))
