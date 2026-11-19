# Rescue v1.20.0

7zとMicrosoft Cabinet（CAB）を、ファイル名やディレクトリの管理情報を失ったディスクから検索できます。GUIで内容検索を選ぶか、CLIで対象の拡張子を指定します。復元するのはアーカイブそのものです。

```sh
rescue scan --source disk.raw --mode carve --extension 7z --extension cab --work-dir scan --json
```

## 実ファイルの照合

| 試験 | 結果 |
| --- | --- |
| FAT32から実際に削除した7z | 9パターンを通常検索と索引検索で各9/9件復元。圧縮されたままの全バイトとSHA-256が原本と一致し、7-Zipで展開した各メンバーも元ファイルと一致 |
| ext4から実際に削除したCAB | 9パターンを通常検索と索引検索で各9/9件復元。全バイトとSHA-256が一致し、libmspackでも各メンバーを確認 |
| Windowsで作成したCAB | MakeCABが作成したMSZIP、32 KiB・256 KiB・2 MiBウィンドウのLZXを含む4ファイル。各6メンバーをWindowsのExpandとLinuxのcabextractで照合。複数フォルダー、32 KiBのブロックをまたぐメンバー、空ファイルを含む |
| GUI | FAT32から削除した18アーカイブをZIPに復元。148,851バイトすべてと各SHA-256が一致し、CSV・HTMLの18行も照合 |
| 読み取れない入力 | 7zの12例、CABの24例で破損や非対応形式を識別。取得失敗範囲をまたぐアーカイブを除外し、読み取り可能な別のアーカイブは復元 |

[7zの原本と再現手順](../tests/sevenz/README.md) · [CABの原本と再現手順](../tests/cab/README.md) · [GUIの結果と画像](gui-v1.20-results.json)

## 対応条件

7zでは各ヘッダーと展開メンバーのCRCを検証します。無圧縮・LZMA・LZMA2・Deflate・BZip2、ソリッド圧縮、BCJ・Deltaフィルターを実証しました。CABでは無圧縮・MSZIP・LZXの各ブロックを読み、チェックサム、展開長とメンバーの配置を検証します。対応形式の取り出しに外部プログラムは不要です。

内容検索の対象は連続配置です。元ファイル名・削除の事実・署名者の真正性は、この検査からは特定できません。暗号化・分割7z、Quantumや分巻CAB、署名付きCAB、上限を超えるファイルには、必要な鍵や全巻と、対応する外部ツールが必要です。[各形式の上限・手順](IMAGING.md)
