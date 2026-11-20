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

## Linuxカーネルで削除した場合

Linux 6.12.107の隔離したVMでも、同じ16本を別々のNTFS、FAT32、ext4ディスクの `shoot` ディレクトリーにコピーし、同期して全バイトを読み戻しました。その後 `rm` で16本を削除し、ディレクトリーが空になったことを確認してアンマウントしました。ゲストが記録したディスク全体のSHA-256をホスト側の各RAWと照合し、公開版v1.37.0で次の結果を得ました。[Linuxカーネルで削除した48件の結果](public-v1.37-linux-kernel.json)

| Linuxドライバー | 復元した原本 | 観察結果 |
| --- | ---: | --- |
| VFAT（FAT32） | 16/16 | Sleuth Kit経由と内蔵FAT連続配置推定の両方で全バイト・SHA-256が一致 |
| ext4 | 16/16 | 現在のinodeを使う一覧は0件。内蔵の `ext4-journal` が過去の名前と配置を16件検出し、既知の原本SHA-256を指定して全バイトが一致。不一致のSHA-256では出力を作成せず停止 |
| ntfs3（NTFS） | メタデータから0/16 | 内蔵NTFSとSleuth Kitの一覧はともに0件。これらの経路ではファイルを指定して復元できなかった |

3つの試験ディスクはそれぞれ192 MiB、ext4とNTFSのブロックまたはクラスタは4 KiB、FAT32は1 KiBクラスタです。ファイルサイズは約1.5 MBから20.8 MBです。3つのRAWすべてで検証前後のSHA-256が一致しました。

ext4で同じ確認を行うには、一覧にある過去のinode番号と、別に保管している完全な原本のSHA-256を指定します。

```sh
rescue ext4-journal --source ext4.raw --json
rescue ext4-journal --source ext4.raw --inode INODE --sha256 ORIGINAL_SHA256 --output restored.dng --json
```

意図的に断片化してからカーネルで削除した場合は、同じ16形式でも結果が変わりました。[断片化したNTFS・FAT32での32件の実測と再現手順](kernel-fragments/README.md)

exFATの連続配置と断片化を同じ原本で試した結果は、[Linuxで削除したexFATカメラカード32件](kernel-exfat/README.md)に記録しました。
