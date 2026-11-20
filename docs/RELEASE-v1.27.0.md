# Rescue v1.27.0

ファイルシステムの名前や管理情報が消えても、連続した領域に残るHEIC・AVIF静止画を、画像の構造から探して復元できます。GUIのシグネチャ探索、CLI、MCPで使えます。復元には外部デコーダーのインストールは不要です。

```sh
rescue scan --source card.raw --mode carve --extension heic --extension avif --work-dir scan --json
rescue results --session <session> --json
rescue recover --session <session> --id <id> --destination recovered --json
```

HEICを探す場合は `--extension heif` も使えます。主画像、タイル、サムネイル、補助画像の参照と、各項目の記録位置を検査します。HEVCは構成情報とNAL単位、AV1は構成情報とOBU単位の長さを確認します。Exif・XMPは画像に含まれるバイト列を保持します。

## 原本との照合

| 実証したファイル | 件数 | 含まれる条件 |
| --- | ---: | --- |
| HEIC | 14 | 奇数の画像寸法、アルファ、10ビット、複数画像、2×2タイル、サムネイル、Exif、XMP、Apple ImageIOが書き出した5画像 |
| AVIF | 7 | カラー、アルファ、10ビット、グレースケール、FFmpeg、Exif・XMP付き画像 |

libheifのx265・libaom、FFmpeg、macOSのApple ImageIOで作成した21件をFAT32・ext4に書き込み、削除前に読み戻して原本と照合しました。実際に削除した後、通常・インデックス付きの両スキャンで各21件を復元し、全バイト・SHA-256と独立デコーダーの画像出力を照合しました。JPEGから引き継いだExif・XMPの5件では、独立したlibheif APIでタグ本体を取り出し、元JPEGに保存したメタデータとも照合しました。libheif 1.19.8とUbuntu 22.04の1.12.0で検証しました。[原本ハッシュと測定結果](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.27.0/docs/heif-v1.27-results.json)

macOSの `sips` で横長・縦長・アルファ・グレースケール・高ビット深度入力の5画像をHEICに変換し、Apple ImageIOで再度開きました。同じ5原本をUbuntuでも独立してデコードし、復元前後のサンプル値を照合しました。[macOSでの原本作成](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44652074819) · [Ubuntuでの独立デコード](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44653370200)

隣接する画像を別々に復元し、1 MiBのスキャン境界をまたぐ先頭も検出しました。破損・未対応22条件で候補を復元対象から除きました。取得マップに読めないブロックがある場合は、その直前で完結する画像を復元し、欠損にかかる画像を除外しました。

Linuxの実GUIでもFAT32の削除画像16件（92,330バイト）を選択してZIPに復元し、各エントリーを元ファイルと全バイト照合しました。画面から書き出したHTML・CSVの内容も復元結果と一致しました。Rustテスト96件、厳格なClippy、統合試験84組が通過しました。従来の35形式・36原本の比較試験では1,116件中1,016件が原本に完全一致し、欠損や上書きを含めて期待値どおりでした。[GUIの測定記録](gui-v1.27-results.json)

## 復元できる範囲

対象は `ftyp`・画像項目を記録した `meta`・明示長の `mdat` がこの順に並び、各項目の範囲をローカルデータに対応付けられる64 MiB以下の静止画です。選択範囲、項目数（最大4,096）、画像寸法、重複、メディアデータ全体の範囲を検査します。1項目が複数の離れた領域を使う配置、外部参照、保護された項目、時間軸付きシーケンスは今回のシグネチャ探索の対象外です。

復元範囲は参照済みの `mdat` の末尾までです。その後に続く任意のボックス、元の名前、削除状態、画素の正しさを構造だけで確定することはできません。元のサイズ・ハッシュ・ファイルシステムの管理情報があれば照合に利用できます。上書きやTRIMで消失したバイトの復元には別のコピーが必要です。

検証用の実削除ディスクと独立デコードの再現にはmtools、e2fsprogs、The Sleuth Kit、libheifを使用します。試験用画像の再生成にはlibheif、x265、libaom、FFmpeg、カメラタグ付きJPEGの作成にはPillowを使用します。
