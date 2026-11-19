# Rescue v1.21.0

原本のサイズとSHA-256を使う検索をGUIとCLIに追加しました。ファイル名、拡張子、先頭の識別子を失っていても連続領域を検索できます。削除されたディレクトリエントリー等から先頭位置も分かる場合は、2か所に分かれた領域を前後両方向に探します。結果には照合した領域のオフセットと長さが記録されます。

GUIでは復元元のディスクやイメージを選び、「ディスクのツール」から「原本のSHA-256でファイルを復元」を開きます。CLIでは次のように指定します。位置とサイズの単位はバイトです。

```sh
rescue recover-by-hash --source disk.raw --sha256 "$SHA256" --file-bytes 12500 --block-size 4096 --start 1048576 --end 16777216 --first-offset 3145728 --output restored.bin --json
```

## 削除から復元までの実測

| 媒体 | 操作 | 原本との照合 |
| --- | --- | --- |
| FAT12、512バイトクラスタ | 離れた2領域を割り当て、7ファイルを削除 | 全7件でバイト列・SHA-256・記録済みの2領域が一致 |
| FAT16、1,024バイトクラスタ | 同じ7原本を異なるクラスタサイズで割り当てて削除 | 全7件で3項目が一致 |
| FAT32、512バイトクラスタ | 同じ7原本を割り当てて削除 | 全7件で3項目が一致 |
| デスクトップGUI | FAT12の削除済み7ファイルを個別に検索して保存 | 全7件で原本のバイト列・SHA-256が一致。異なるダイジェストでは出力なし |

各媒体で `mcopy` による書き込み後、`mtype` で内容を読み、`mshowfat` で削除前の配置を独立に記録しました。`mdel` で削除し、検索には先頭位置だけを渡しました。対象は日本語テキスト、CSV、HTML、PCM WAV、ZIP、独自バイナリ、同じブロックを繰り返すデータです。追加試験では、先頭位置が不明な連続配置、2番目の領域が先頭より手前にある配置、RAID0、取得失敗セクターの除外、検索量の上限と同時実行時の保存動作を確認しました。[再現スクリプト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.21.0/scripts/e2e_whole_hash.py) · [実測値とGUI画像のダイジェスト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.21.0/docs/whole-hash-v1.21-results.json) · [検索画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.21.0/docs/images/v1.21.0/whole-hash-search.png)

原本と一致した各領域をもう一度読み取り、最終出力のSHA-256を検証します。内容の一致から、削除済みだったか、同一内容の複数コピーのどれか、元の名前やパスは特定できません。3領域以上への断片化、上書きやTRIMで失われた内容、復号されていない暗号化領域は、この検索からは復元できません。通常の復元に外部ツールは不要です。実削除の再現にはmtoolsを使います。[サイズ・検索上限と操作手順](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.21.0/docs/IMAGING.md#原本のsha-256とサイズから探す)

同じGUIビルドで、FAT32から実際に削除した18アーカイブのZIP復元、CSV・HTMLレポート、64 MiBイメージの取得中断と再開を検証しました。[GUI検証記録](gui-v1.21-results.json)
