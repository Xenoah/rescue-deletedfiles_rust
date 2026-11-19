# Rescue v1.8.0

GUIから仮想ディスクや証拠イメージをRAWに変換して、そのまま復元元に設定できます。AppleのDMG（UDIF）はアプリ内で展開します。削除管理情報が消失した媒体では、OpenDocument・EPUB・JSONの内容から復元候補を探せます。

## ディスクイメージを開く

「設定」→「保存したイメージ」→「ディスクのツール」→「イメージをRAWに変換」で元ファイルと新しいRAWの保存先を選びます。変換完了時には、論理ディスクのバイト数とSHA-256を表示します。中断時の一時出力は破棄し、既存の保存先は保持します。

| 入力 | 読み取り条件 |
| --- | --- |
| DMG（UDIF） | 単一セグメントとXMLブロック表。非圧縮、ゼロ領域、ADC、zlib、bzip2。記録されたCRC-32と領域の境界を照合。暗号化・旧リソースフォーク・未対応圧縮方式は対象外 |
| VDI・QCOW2 | 単独の対応バージョンをアプリ内で展開。親ディスクに依存する構成は対象外 |
| VHD・VHDX・VMDK・AFF・分割RAW | 対応ドライバーを含むThe Sleuth Kitの `img_cat` を利用 |
| E01・分割E01 | libewfで取得エラーを確認し、論理ディスクのハッシュを照合。分割イメージは全セグメントを順に指定 |

## ファイルの内容から探す

- **OpenDocument・EPUB**は、ZIP各エントリーの長さとCRCを照合し、mimetype、manifest、EPUBのOPFなどを検証します。文書、表計算、プレゼンテーション、図形、テンプレートなど17種類のOpenDocument拡張子とEPUBを分類できます。判別できないパッケージも、有効なZIPとして復元できる場合があります。
- **JSON**はUTF-8のオブジェクト・配列を4 MiBまで検証します。先頭のBOMを保持します。空オブジェクトや数値・真偽値などの配列も探す場合は `--extension json` を指定します。検出できる終端は対応する閉じ括弧までです。

```sh
rescue import-image --source mac.dmg --image-type dmg --output mac.raw --json
rescue scan --source mac.raw --mode carve --extension odt --extension epub --extension json --work-dir sessions --json
```

## 実証結果

- GUIでVDI・QCOW2・VHD・VHDX・VMDK・AFF・5分割E01・4分割RAWを変換しました。各16 MiBの論理ディスク全体と、各ディスクのext4上で削除した7ファイルが一致しました。計56ファイルです。[GUIで試験した入力とハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.8.0/docs/gui-disk-import.json)
- 6条件のDMGから各7ファイルを復元しました。さらに、libdmg-hfsplusで作成した68 MiBのDMGをGUIで変換し、APM上のFAT32からmtoolsで削除したPNG・WebP・ICO・テキスト・XML・ZIP・bzip2を復元しました。7条件とも論理ディスク全体とファイルのSHA-256が一致しました。破損・未対応など17条件では不完全なRAWを確定しませんでした。公開されているRectangle 2.0.4のDMGでも、展開後の14,899,712バイトがQEMU・dmg2img両方の出力に一致しました。[独立ツールで作成した試験イメージ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.8.0/tests/udif/manifest.json)
- odfpy・Pandoc・EbookLibで作った文書・電子書籍21ファイルと、文字コード・エスケープ・入れ子などが異なるJSON 12ファイルを通常・索引の両方で復元し、66件すべてが原本に一致しました。旧版の通常・索引セッションに汎用ZIPとして保存されたEPUBも復元できました。[文書の原本とハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/v1.8.0/tests/open-documents)
- Linux GUIでODT・ODS・ODP・ODG・EPUB・JSONの6ファイルを選択して復元し、ZIP内の各ファイルとHTML・CSVレポートを照合しました。64 MiBのディスク取得では、読み取りを途中で中断してから再開し、全バイトを照合しました。[GUI実操作の記録](gui-v1.8-results.json)
- 35形式・36原本の共通試験でNTFS管理情報を消した場合、連続配置で31/36件、断片化配置で11/36件が原本に一致しました。管理情報を利用する8条件ではCLIとMCPで576/576件が一致しました。上書きした条件では原本との一致は0件でした。
