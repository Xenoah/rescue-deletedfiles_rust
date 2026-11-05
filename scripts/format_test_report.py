"""Turn executed observations into a portable report; never invent test results."""
import argparse
import base64
from collections import Counter
import hashlib
from html import escape
import json
from pathlib import Path
import re
import shutil
import subprocess
import zipfile


def summarize(result):
    cases={}
    for row in result['rows']:
        values=cases.setdefault(row['case'],{'exact':0,'total':0})
        values['exact']+=int(row['byte_exact']);values['total']+=1
    return cases


def report(inputs,output,tag,screenshot):
    output.mkdir(parents=True,exist_ok=True)
    data=[json.loads(p.read_text(encoding='utf-8')) for p in inputs]
    for item in data:
        assert item['formats']==35 and item['corpus_files']==36
        assert all(r['byte_exact'] for r in item['rows'] if r['expected']=='byte_exact')
    combined={'release_tag':tag,'test_commit':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),'runs':data}
    (output/'format-verification.json').write_text(json.dumps(combined,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    lines=[f'# {tag} クラウド復元テスト', '', '35種類・36ファイルの人工生成コーパス。全世界のファイル形式・全媒体の成功を意味しない。',
           '管理情報が残る条件と、実際の削除・管理情報喪失・上書きの条件を分けて集計する。', '',
           '| 実行環境 | 条件 | バイト一致 / 比較件数 |','| --- | --- | --- |']
    for item in data:
        label=('公開Windows版' if item.get('published_release') else item['platform'])
        for case,count in summarize(item).items():lines.append(f"| {label} | {case} | {count['exact']} / {count['total']} |")
    lines+=['','FAT32の実削除はmtoolsのmdel。NTFS/exFATの独立メタデータ試験は削除状態を明示的に合成している。',
            '管理情報喪失後の探索は結果表に記録した連続配置の画像のみ。断片の再構成や全形式のcarvingは未完了。',
            'SSDの実TRIM・物理故障・USB/SD実機・RAID・有料製品との比較・Windows GUI手動操作は未検証。',
            '', '全ファイルの元SHA-256・出力SHA-256は同梱JSONに収録。結果の集計はこのテストセットに限る。']
    (output/'format-verification.md').write_text('\n'.join(lines)+'\n',encoding='utf-8')
    if screenshot and screenshot.is_file():
        gui=next((run for run in data if run.get('gui_screenshot_sha256')),None)
        assert gui is not None,'GUI image requires its executed verification record'
        assert hashlib.sha256(screenshot.read_bytes()).hexdigest()==gui['gui_screenshot_sha256']
        hashes={row['file']:row['original_sha256'] for row in data[0]['rows']}
        assert all(hashes[row['file']]==row['output_sha256'] for row in gui['rows'])
    image=''
    if screenshot and screenshot.is_file():
        encoded=base64.b64encode(screenshot.read_bytes()).decode('ascii')
        image=f'<figure><img alt="Linux GUIで36ファイルを保存し正解と照合した実画面" src="data:image/png;base64,{encoded}"><figcaption>実GUI：Linux/Xvfbの合成イメージで36ファイルを保存。Windows GUIの手動試験ではありません。</figcaption></figure>'
    payload=json.dumps(combined,ensure_ascii=False).replace('<','\\u003c')
    html='''<!doctype html><html lang="ja"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Rescue Cloud Verification</title><style>
body{margin:0;background:#0b121a;color:#edf5fa;font:16px/1.7 system-ui,sans-serif}main{max-width:1200px;margin:auto;padding:32px 24px}h1{font-size:36px}.muted,figcaption{color:#a3b7c7}.notice{border-left:3px solid #f2ca82;padding:12px;background:#272217}.grid{display:grid;grid-template-columns:repeat(3,1fr);gap:12px}.card{background:#12212c;border:1px solid #2a414f;border-radius:8px;padding:18px}.number{font-size:28px;color:#7ddbc5}select,input{font:inherit;max-width:100%;padding:8px;background:#162936;color:white;border:1px solid #3a5a69;border-radius:6px}label{display:inline-block;margin:8px 14px 8px 0}table{width:100%;border-collapse:collapse;font-size:13px}th,td{border-bottom:1px solid #2a414f;text-align:left;padding:9px;vertical-align:top}th{color:#7ddbc5}code{overflow-wrap:anywhere}.scroll{overflow-x:auto}img{width:100%;height:auto;border:1px solid #2a414f;border-radius:8px}figure{margin:28px 0}details{padding:12px 0}summary{cursor:pointer}a{color:#7ddbc5}.ok{color:#7ddbc5}.no{color:#f2ca82}@media(max-width:700px){.grid{grid-template-columns:1fr}h1{font-size:28px}main{padding:22px 15px}}
</style><main><div class="muted">RESCUE / CLOUD RECOVERY VERIFICATION</div><h1>復元できた条件を、結果から確認。</h1><p>__TAG__ · 35種類・36ファイル。管理情報が残る条件、実際の削除、管理情報喪失を分けたテスト結果です。</p><p class="notice">元データと全バイト・SHA-256が一致した件数を表示します。この小さな人工データセットの結果は、実媒体や有料ソフトに対する一般的な復元率ではありません。</p>
<div class="grid"><div class="card"><div class="number">35 種類</div>画像・文書・音声・動画・圧縮・DB</div><div class="card"><div class="number">36 ファイル</div>独立エンコーダーで生成・再読込</div><div class="card"><div class="number">CLI / MCP / GUI</div>GUIはLinuxで36件を別途照合</div></div>
<h2>実行環境・条件別の結果</h2><label>実行環境 <select id="run"></select></label><div class="scroll"><table><thead><tr><th>条件</th><th>バイト一致</th><th>比較件数</th></tr></thead><tbody id="summary"></tbody></table></div>
<h2>ファイル形式ごとの観測結果</h2><label>条件 <select id="case"></select></label><label>形式で絞込 <input id="filter" aria-label="形式で絞込" placeholder="PNG / DOCX / MP4"></label><div class="scroll"><table><thead><tr><th>形式</th><th>元ファイル</th><th>操作</th><th>保存状態</th><th>全バイト一致</th><th>出力 SHA-256</th></tr></thead><tbody id="rows"></tbody></table></div>
<details><summary>条件と未検証範囲</summary><p>NTFS/exFATの独立試験は独立ツールの管理情報に削除状態を合成。FAT32の実削除はmtoolsのmdelです。上書き前提の試験は、データ領域を明示的にゼロへ置き換えたものです。SSDのTRIM実行を検証したものではありません。</p><p>管理情報喪失後の対応形式はこの版の結果表で確認できます。断片再構成、RAID、物理故障、有料ソフト比較、Windows GUI手動操作はこの試験の対象外です。</p></details>__IMAGE__<p class="muted">テストの元ハッシュと出力ハッシュをJSONにも同梱。入力は生成した通常ファイルのイメージだけです。この説明UIは通信せず動作します。</p></main>
<script>const evidence=__DATA__;const r=document.getElementById('run'),c=document.getElementById('case'),f=document.getElementById('filter');evidence.runs.forEach((v,i)=>{const o=document.createElement('option');o.value=i;o.textContent=v.published_release?'公開Windows版 '+v.published_release.tag:v.platform;r.append(o)});function cell(row,text,cls){const td=document.createElement('td');td.textContent=text;if(cls)td.className=cls;row.append(td)}function choose(){const v=evidence.runs[Number(r.value)];const cases=[...new Set(v.rows.map(x=>x.case))];c.replaceChildren();cases.forEach(name=>{const o=document.createElement('option');o.textContent=name;c.append(o)});const body=document.getElementById('summary');body.replaceChildren();cases.forEach(name=>{const items=v.rows.filter(x=>x.case===name),tr=document.createElement('tr');cell(tr,name);cell(tr,items.filter(x=>x.byte_exact).length);cell(tr,items.length);body.append(tr)});draw()}function draw(){const v=evidence.runs[Number(r.value)],body=document.getElementById('rows');body.replaceChildren();v.rows.filter(x=>x.case===c.value&&x.format.toLowerCase().includes(f.value.toLowerCase())).forEach(x=>{const tr=document.createElement('tr');cell(tr,x.format);cell(tr,x.file);cell(tr,x.frontend);cell(tr,x.status);cell(tr,x.byte_exact?'一致':'一致せず',x.byte_exact?'ok':'no');cell(tr,x.output_sha256||'—');body.append(tr)})}r.addEventListener('change',choose);c.addEventListener('change',draw);f.addEventListener('input',draw);choose();</script></html>'''
    html=html.replace('__TAG__',escape(tag)).replace('__DATA__',payload).replace('__IMAGE__',image)
    (output/'format-verification.html').write_text(html,encoding='utf-8')
    with zipfile.ZipFile(output/'format-verification.zip','w',zipfile.ZIP_DEFLATED) as z:
        for name in ['format-verification.json','format-verification.md','format-verification.html']:z.write(output/name,name)
        if screenshot and screenshot.is_file():z.write(screenshot,'format-matrix.png')
    names=['format-verification.json','format-verification.md','format-verification.html','format-verification.zip']
    if screenshot and screenshot.is_file():
        shutil.copyfile(screenshot,output/'format-matrix.png');names.append('format-matrix.png')
    sums=''.join(f"{hashlib.sha256((output/name).read_bytes()).hexdigest()}  {name}\n" for name in names)
    (output/'FORMAT-SHA256SUMS.txt').write_text(sums,encoding='ascii')
    print('\n'.join(lines))


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--input',type=Path,action='append',required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--tag',required=True);p.add_argument('--screenshot',type=Path)
    a=p.parse_args();report(a.input,a.output,a.tag,a.screenshot)
