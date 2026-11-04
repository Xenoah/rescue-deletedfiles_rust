"""Append executed cloud verification to existing notes, preserving their body."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re

p=argparse.ArgumentParser();p.add_argument('--tag',required=True);p.add_argument('--report',type=Path,required=True)
p.add_argument('--body',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
if not re.fullmatch(r'v\d+\.\d+\.\d+-alpha\.\d+',a.tag):raise SystemExit('invalid release tag')
data=json.loads(a.report.read_text(encoding='utf-8'))
windows=next(run for run in data['runs'] if run.get('published_release'))
assert windows['published_release']['tag']==a.tag
assert all(row['byte_exact'] for row in windows['rows'] if row['expected']=='byte_exact')
base='https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/'+a.tag+'/'
section=f'''<!-- cloud-format-verification:start -->
## クラウドでの全形式テスト

35種類・36ファイルを生成し、元データとの全バイト一致・SHA-256で照合しました。
NTFSの連続配置・断片化・圧縮・ADS、FAT32/exFATの配置方式をCLI/MCPで検証。
公開済みのWindows版ZIPをチェックサム照合後に取得し、同梱のrescue.exeそのものでも実行しています。

実削除したFAT32や、管理情報喪失・上書きは別条件として集計しています。全条件での復元成功を意味しません。
詳細・形式別の結果は[操作できる検証レポート]({base}format-verification.html)、
[ハッシュ付き検証一式]({base}format-verification.zip)を保存して確認できます。

![全36ファイルの保存・照合を行ったLinux GUIの実画面]({base}format-matrix.png)

Windows GUI手動操作、SSD実TRIM、物理故障、実デバイス、RAID、有料製品比較はこのテストの対象外です。
<!-- cloud-format-verification:end -->'''
body=a.body.read_text(encoding='utf-8')
body=re.sub(r'<!-- cloud-format-verification:start -->.*?<!-- cloud-format-verification:end -->','',body,flags=re.S).rstrip()
a.output.write_text(body+'\n\n'+section+'\n',encoding='utf-8')
