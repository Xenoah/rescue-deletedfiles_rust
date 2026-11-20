# Rescue v1.50.0

ディスク全体のイメージから、パーティション先頭を起点として記録した配置で削除ファイルを復元できます。マウント中にLinuxのFIEMAP・FIBMAPなどで得た物理位置を、ディスク全体の位置に手作業で足し直す必要がなくなります。MBRの基本・論理パーティション、512バイト・4 KiBセクタのGPT、Apple Partition Mapで検証しました。

## 使い方

`rescue inspect --source disk.raw --json` で `layout.regions[].index` を確認します。原本のSHA-256と削除前の各区間を含む配置表に `"schema_version": 2` と `"partition": 2` のように対象番号を指定してください。GUIでは「配置表からファイルを復元」で同じJSONを選びます。ディスク先頭を起点とする旧形式の配置表も使用できます。[配置表の書き方](IMAGING.md#任意の形式を既知の配置とsha-256から復元する)

```sh
rescue recover-extents --source disk.raw --layout capture.json --output restored.dng --json
```

## 実際の削除と照合

| 検証したディスク | 原本と全バイト・SHA-256が一致した件数 |
| --- | ---: |
| 16種類のカメラRAWを含むNTFS、FAT32、連続配置のexFAT、断片化したexFAT。GNU PartedでGPT・MBR論理・Apple Partition Map・MBR基本の各ディスクに配置 | 各16件、計64件 |
| Linuxカーネルで10形式を断片化して削除したNTFSとFAT32。4 KiBセクタのGPT第2パーティションとMBR論理パーティション5に配置 | 各10件、計20件 |
| GNU Parted製のGPTで4 GiB地点をまたぐext3。WAVの先頭と末尾が境界の両側に位置 | 9件 |

NTFS・FAT32ではAVI、DOCX、FLAC、OGG、PPTX、SQLite、TAR、TIFF、WAV、XLSXを1 KiBずつ交互に書き、各ファイルが4～29区間に分かれたことをカーネルで確認しました。全ファイルを読み戻してから削除しています。逆順にした20件の配置表は出力を作らず拒否しました。公開済みv1.37.0のLinux CLIに同じFAT32ディスクと形式2の配置表を渡すと、10件すべてが未対応として拒否されました。[カメラRAWの詳細](../tests/partition-extents/camera-results.json) ｜ [削除後のディスク・配置・再現手順](../tests/partition-extents) ｜ [4 GiB境界の測定値](partition-extent-4g.json)

Linuxの実GUIでもGPT第2パーティションから10形式を復元し、全出力を原本と直接比較しました。原本のSHA-256を変えると出力を作らずエラーを表示しました。[GUIの結果と画面](partition-extent-gui.json)

## 復元できる条件

指定した区間のバイトが残り、並びと原本のSHA-256が分かっている必要があります。ファイルシステムのブートセクターの欠損はこの方法で扱えます。パーティション表を解決できない場合、選んだパーティションの末尾を越える場合、要求したデータ範囲が取得マップで欠損している場合、全体のハッシュが違う場合はいずれも保存を拒否します。スパースファイルの既知の穴は明示的に指定できます。[範囲・破損試験の結果](partition-extent-results.json)

配置を採取せずに削除した任意の断片化ファイルや、TRIM・上書き済みのデータの原本は、この方法だけでは復元できません。暗号化や圧縮によって得られた論理アドレスは、対象ボリュームとその形式に対応する手段で平文のバイト位置に変換する必要があります。
