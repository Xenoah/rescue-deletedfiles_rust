# カメラRAWの削除・復元試験

公開版Rescue v1.37.0のLinux配布物で、16形式の実カメラ原本をFAT32とext4に書き込み、各ボリュームから読み戻して原本と照合してから削除しました。16本の合計は110,844,883バイトです。各ディスクで16本ともファイル全体とSHA-256が一致し、復元前後のディスクのSHA-256も一致しました。[全32件の結果](public-v1.37-linux.json)

| 対象 | 削除 | 復元経路 |
| --- | --- | --- |
| FAT32 | mtools の `mdel` | `rescue filesystem` と、連続配置推定を明示した内蔵FAT復元の両方で16件一致 |
| ext4 | e2fsprogs の `debugfs rm` | `rescue filesystem` からSleuth Kitを使用して16件一致 |

対象形式はARI、ARW、CR2、CR3、CRW、DCR、DNG、ERF、GPR、MRW、NEF、ORF、PEF、RAF、RW2、SRWです。raw.pixls.usでCC0と表示されている原本を使用しています。機種名、公開URL、サイズ、SHA-256を[原本一覧](sources.json)に記録しました。

この試験はファイル配置情報を使用します。FAT32では削除時に複数クラスタのチェーンが消え、通常設定では16件すべてが復元対象外になりました。連続配置を明示的に仮定した出力は `estimated_contiguous` と記録されます。元の配置が断片化している場合は同じ結果を保証できず、全体の一致を確かめるには原本のハッシュなど独立した証拠が必要です。ext4の削除後に残る配置情報は削除方法・上書き・ジャーナル状態に依存します。これら16形式のシグネチャ検索は、この試験の対象外です。

## 再現手順

Linux上でPython 3.11以降、dosfstools、mtools、e2fsprogs、Sleuth Kitを使用します。作業用の通常ファイルに2つの試験ディスクを順に生成します。

```sh
mkdir camera-originals
python - <<'PY'
from pathlib import Path
import json, urllib.request
for row in json.loads(Path('tests/camera-raw/sources.json').read_text()):
    urllib.request.urlretrieve(row['source'], Path('camera-originals') / row['name'])
PY
python scripts/verify_camera_raw_metadata.py --binary /path/to/rescue --originals camera-originals
```
