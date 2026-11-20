# Rescue v1.54.0

Linux MDのミラー（RAID1）はメンバー1台から、RAID10 near=2は隣り合う各ミラーペアの1台ずつから、削除ファイルを復元できるようになりました。メンバーを選んで「入力したメンバーからLinux MD構成を読む」を押すか、CLIで構成を保存します。

```sh
rescue raid-md --member surviving-mirror.raw --output mirror.rescue-raid.json --json
rescue scan --source mirror.rescue-raid.json --mode carve --work-dir scan --json
```

4Knの場合は `--sector-size 4096` を指定してください。構成は読み取りに使用したメンバーに結び付きます。壊れたミラーを交換する操作や、元のディスクへの書き込みは必要ありません。

| 実配列での照合 | 結果 |
| --- | --- |
| RAID1を2・3・4台、RAID10 near=2を4台で2組 | 5配列の各ext4で10形式を作成・読み戻し・削除 |
| 生存メンバーの組み合わせ | 全55通り。読める43通りで30区間が原本と一致、ミラーペア全欠損の12通りを拒否 |
| 最少メンバーから復元した全論理ディスク | 5組ともLinuxが正常時に読んだ12 MiBと全バイト・SHA-256が一致 |
| 配置指定と通常の内容検索 | どちらもBMP・FLAC・GIF・JPEG・PDF・PNG・SQLite・WAV・WebP・ZIPの計50件が原本と一致 |
| Linuxで独立に最少メンバーから読み取り専用で起動 | RAID1の9通りとRAID10の8通りで全体SHA-256が一致 |

実GUIでは管理情報1.1、4Knの3台ミラーのうち1台だけを使い、10形式をスキャンしてZIPに保存しました。全31,604バイトと各ファイルのSHA-256、出力レポートの欠損スロットを照合しました。[実GUIの記録](gui-v1.54-results.json)

far=2とoffset=2は実際の配列を作成して拒否を確認しました。別コピーを対応するLinux MDで読み取り専用に組み立て、論理RAWを取得してください。世代が食い違う場合や、読み取れた2つのコピーが違う場合に、Rescueがどちらかを正しいと決めることはできません。唯一のコピーに残った上書きは原本の既知のSHA-256と比較してください。[元データとゲストログ](../tests/md-mirrors) · [Linuxでの照合結果](md-mirror-native.json)
