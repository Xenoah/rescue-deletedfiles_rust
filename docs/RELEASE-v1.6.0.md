# Rescue v1.6.0

旧MacのApple Partition Map（APM）を使うディスク、VHDX・AFF・E01イメージに対応範囲を広げました。ファイル名や削除管理情報を失った媒体では、bzip2・XZ・lzip・Zstandard・LZ4の5形式を検索できます。

## ディスクとイメージ

| 入力 | 機能と条件 |
| --- | --- |
| APM | GUI・CLI・MCPの通常スキャンで各パーティションを検出。512・1024・2048・4096バイトブロックとMacRoman名に対応。APMの検出後、NTFS・FAT・exFATなどの対応ファイルシステムを解析 |
| 固定・可変VHDX、単一AFF | 対応ドライバーを含むThe Sleuth Kitを利用。CLIで直接ファイルシステムを調べるか、論理RAWへ変換 |
| 単一・分割E01 | libewfの取得エラー報告を確認し、論理RAW出力を独立したSHA-256と照合。取得エラーの記録、セグメントの欠落、照合失敗があればRAWを確定しない |

```sh
rescue inspect --source mac-disk.raw --json
rescue filesystem --source guest.vhdx --image-type vhdx --fs-type ext4 --json
rescue import-image --source evidence.E01 --segment evidence.E02 --image-type ewf-verified --output evidence.raw --json
rescue scan --source evidence.raw --mode carve --extension xz --extension zst --extension lz4 --work-dir sessions --json
```

APMはディスクの区画方式です。APM上のHFS・HFS+を認識するための管理情報解析は今回の追加に含みません。VHDX・AFFは読み手が対応する種類に限り、今回実証したのは独立したディスクです。[パーティションの読み取り](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.6.0/docs/DISCOVERY.md) · [E01の取り込み手順](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.6.0/docs/IMAGING.md)

## 圧縮ファイルの復元

5形式とも、圧縮データを復号して終端を検査します。記録された検査値と復号サイズを形式に応じて照合し、元の圧縮バイトを保存します。連結されたメンバー・フレームは個別の候補として扱います。XZの任意の末尾パディングは除きます。ZstandardとLZ4の外部辞書・スキップ用フレームは対象外です。1候補の入力上限は64 MiB、復号上限は128 MiBです。

## 実証

- GNU Partedで作成したAPM上のFAT32で、mtoolsで実際に削除したPNG・WebP・bzip2・XML・テキスト・ICO・ZIPを通常・索引の両方で復元し、14件すべてが原本に一致しました。ブロックサイズを変えた4条件でも、CLIとMCPで計56件が一致しました。
- 固定VHDX、可変VHDX、AFF、単一E01、17分割E01の5条件で展開したRAW全体が16 MiBの原本と一致しました。それぞれのRAWからext4上で削除した7ファイルを復元し、計35件の元バイト列とSHA-256が一致しました。E01の後続セグメントを外した試験では、出力を確定しませんでした。
- bzip2・XZ・lzipの11パターンと、Zstandard・LZ4の10パターンを通常・索引スキャンで照合し、42件すべてで元の圧縮バイト列が一致しました。途中切断や検査値の破損など計18条件では復元対象から外れました。
- 管理情報を消去した35形式・36原本で、連続配置は25/36件、断片化配置は8/36件が原本に一致しました。v1.5.0の同じ条件では23/36件と6/36件でした。上書き・TRIM後や任意の媒体での復元率を示す数値ではありません。
- Linux GUIで追加5形式の7ファイルを選択・復元し、ZIPの全エントリー、HTML・CSVレポートを原本と照合しました。64 MiBの取得では読み取りプロセスの中断後に再開し、全バイトが一致しました。[GUI実操作の記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.6.0/docs/gui-v1.6-results.json)
