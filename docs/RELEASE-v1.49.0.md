# Rescue v1.49.0

削除記録が失われたディスクから、対応するDNG・CinemaDNGをファイル構造で検索できます。TIFFの各ディレクトリー、センサーの配置、全ストリップ・タイルの長さを確認します。ロスレスJPEGは指定画素数の差分符号を終端まで読み、JPEGプレビューもデコードします。この検索はWindows、macOS、LinuxのRescueに内蔵しています。

```sh
rescue scan --source camera-card.raw --mode carve --extension dng --work-dir scan --json
rescue recover --session SESSION --all --destination restored --json
```

## 撮影原本での結果

CC0で公開されている撮影データ10本をLinuxで2枚の試験ディスクに書き込み、全バイトを読み戻した後で削除しました。NTFS（ntfs3）では削除ファイルの名前・配置をメタデータから取得できず、ディスクの内容を直接検索しました。ext4では過去のジャーナルに名前と配置が残っていました。

| ディスクと方法 | 原本全体を復元 | その他の結果 |
| --- | ---: | --- |
| NTFS、通常・索引によるDNG検索 | 6/10 | Blackmagic 2本は最後に参照された範囲まで復元。残り2本は対応外 |
| ext4、過去のジャーナルと原本SHA-256 | 10/10 | 現在のinodeから列挙できた対象は0件 |

同じNTFSディスクを公開版v1.37.0でDNG検索した場合は、通常・索引ともに候補は0件でした。[公開版との比較](dng-public-comparison.json)

NTFSから完全に復元した6本は、Canon EOS 5D Mark IIIの撮影データ2本と、PowerShot・IXUSの異なる4機種の撮影データです。Blackmagicの2本は未参照の末尾1バイト・44バイトがあり、復元した範囲から展開したセンサー画素は両方とも原本と一致しました。完全復元した各ファイルでは全バイトとSHA-256を照合しました。[撮影原本・出典](../tests/dng/cameras.json) · [ディスク・撮影機種別の結果](dng-camera-results.json)

## ストリップ・タイル・圧縮の実証

独立したライブラリーで作った15本にも、大小両エンディアン、4種類のBayer配列、8・10・12・14・16ビット、2種類のロスレスJPEGエンコーダーを含めました。FAT32・ext4で実際に削除した延べ30件が通常・索引の両方で全バイト・SHA-256とも原本と一致し、展開した全センサー値も生成時の配列と一致しました。破損・未対応構造・読めないセクターの32条件では復元を拒否しました。[通常・索引の結果](dng-results.json) · [Linuxの削除実証](dng-native-results.json)

最初の画像ディレクトリーが1,114,112バイト先にあるDNGを、通常TIFFとBigTIFFに隣接させた試験でも、指定した拡張子だけを通常・索引の両方で完全復元できました。[混在ディスクの結果](dng-directory-results.json)

GUIで新しいFAT32スキャンを行い、15本をZIPに復元して展開した104,963バイトを原本と照合しました。[GUIの検証記録](gui-v1.49-results.json)

Linuxのリリースビルドで統合テスト109組、Rustテスト96件、Clippyの検査を通過しました。既存の35形式のコーパス1,116件はすべて期待値どおりで、このうち1,016件が原本と完全一致しました。

## 対応範囲

対象はクラシックTIFFに格納されたDNG 1.0～1.4の単一Bayer画像で、連続した領域に残っているものです。最大64 MiBの入力と、プレビュー・タイル端を含めて合計33,554,432サンプルまでを検証します。未参照の末尾や元の名前にはファイル配置情報が必要です。断片化、上書き、暗号化、未知のMakerNote、Lossy DNG、JPEG XL、LinearRawにはこの検索を適用できません。

CR2・CR3・ARW・NEFを含む撮影原本16形式については、既存のファイルシステム経由の復元も公開版で実証しました。ext4の過去のジャーナルは内蔵機能です。`rescue filesystem` にはSleuth Kitが必要です。[16形式と復元方法](../tests/camera-raw/README.md) · [DNGの再現手順](../tests/dng/README.md)
