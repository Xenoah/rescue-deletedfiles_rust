# 断片化したF2FSの削除ファイル

Linux 6.12.107上でBMP・JPEG・PNG・SQLite・WAV・ZIPの6原本を4 KiBずつ交互に書き込み、書き込みの都度fsyncしました。F2FSは `nodiscard,noinline_data` でマウントしています。ファイルの内容を読み戻して比較し、LinuxのFIEMAPで削除前の配置を記録してから、6ファイルを削除・同期・アンマウントしました。128 MiBのイメージ内に元ファイル全体が連続して現れないことも確認しました。[書き込み・FIEMAPのコード](write_and_map.c) · [実行したコマンド](guest.sh) · [Linuxのログ](guest.txt.gz)

| 形式 | 削除前サイズ | FIEMAPの範囲数 |
| --- | ---: | ---: |
| BMP | 139,666 | 35 |
| JPEG | 29,293 | 8 |
| PNG | 139,620 | 35 |
| SQLite | 253,952 | 36 |
| WAV | 120,044 | 30 |
| ZIP | 139,357 | 35 |

公開版v1.46.0の通常・索引の内容検索は、いずれも候補6件を返しましたが、削除前の全バイト・SHA-256と一致したファイルは0件でした。以下の2通りでは、6件・合計821,932バイトすべてが削除前の原本と一致しました。

- 削除前に記録したFIEMAPの物理配置と原本SHA-256を `recover-extents` に渡す。
- 削除前の原本から4 KiBごとのフィンガープリントを作り、`recover-fingerprinted` で全ディスクを検索する。配置や開始位置は渡していません。

削除前の配置と各ブロックのハッシュを使わず、ファイル全体のSHA-256・サイズと先頭シグネチャだけを使う方法も試しました。4 KiB境界にあるヘッダーを探し、1ブロックずつ最大8ブロック間隔の規則的な配置を試すと、等間隔に並んだJPEGの1件だけが一致しました。残り5件には途中で間隔が変わる部分があります。[規則的な配置の試行結果](header-stride.json)

```sh
python scripts/verify_fragmented_f2fs.py --binary /path/to/rescue --output fragmented-result.json
python tests/f2fs-linux/fragmented/whole_hash_trial.py --binary /path/to/rescue --output stride-result.json
```

FIEMAPは削除前に取得したものです。削除後にF2FSから配置を自動発見できることを示す試験ではありません。回収時に配置も事前のフィンガープリントもない場合、断片化した任意ファイルの完全復元は保証できません。[元ファイルと位置・SHA-256](manifest.json) · [公開版の照合](published-v1.46-linux.json) · [v1.55.0候補の照合](candidate-v1.55-linux.json)
