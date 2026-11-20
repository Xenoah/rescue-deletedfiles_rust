# Rescue v1.31.0

PhotoshopのPSD・PSBを、ファイルシステムの管理情報が失われた連続領域から復元できます。GUIでは「Carving」で拡張子に `psd` または `psb` を指定します。CLIとMCPでも同じ検査を使います。

```sh
rescue scan --source disk.raw --mode carve --extension psd --extension psb --work-dir scan --json
rescue results --session <session> --json
rescue recover --session <session> --id <id> --destination recovered --json
```

## 削除したレイヤー付き文書の検証

GIMP、ImageMagick、psd-toolsで作成・保存したPSD 17件とPSB 6件を、FAT32とext4のディスクイメージに書き込みました。それぞれで書き込み後のファイルを読み戻し、削除してから通常・索引付きの両スキャンで復元しました。両ディスクで23件すべての全バイトとSHA-256が原本と一致しました。psd-toolsでも復元物を開き、合成画像の画素、個々のレイヤーの名前・矩形・チャンネル別画素、マスクを原本と照合しました。[23原本と独立デコード結果](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.31.0/tests/photoshop/manifest.json) · [復元の測定結果](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.31.0/docs/photoshop-results.json)

| 検証した条件 | 原本に含まれる例 |
| --- | --- |
| 画素 | 1ビットのビットマップ、グレースケール、RGB、アルファ付きRGB、CMYK、16ビット整数、32ビット浮動小数点 |
| 圧縮 | 非圧縮、行ごとのPackBits、ZIP、予測付きZIP。PSDとPSBで異なる行長・セクション長を検査 |
| レイヤー | 大きさと位置が異なるレイヤー、日本語名、グループ、ユーザーマスク、16ビット用の追加レイヤーブロック |
| 境界と欠損 | 1 MiBの走査境界をまたぐ文書、隙間なく並ぶ23文書、未取得範囲の直前で終わる文書、未取得範囲をまたぐ文書 |

ヘッダー・リソース・レイヤー・圧縮データを壊した28例はすべて候補を復元不可と判定しました。合成画像が正常でもレイヤーやマスクだけが破損している例を含みます。取得マップに欠損がある場合、その直前で完結した文書だけを復元しました。LinuxのGUIでもFAT32から23件、計148,591バイトをZIPに復元し、ZIP内の各ファイルを原本と全バイト照合しました。[GUI実測と画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.31.0/docs/gui-v1.31-results.json)

Rustテスト96件と統合試験89組が通過しました。35形式・36原本の比較試験では、上書きなどを含む1,116条件のうち1,016条件で原本と完全一致し、全条件で期待どおりの結果が得られました。

## 復元できる条件

- 復元時にPhotoshop、GIMP、ImageMagick、psd-toolsをインストールする必要はありません。今回の実証ではFAT32の作成・削除にdosfstoolsとmtools、ext4にe2fsprogs、削除記録の確認にThe Sleuth Kitを使いました。
- 対象は連続したPSD v1またはPSB v2です。符号化された文書全体は64 MiBまで、展開される合成画像とレイヤーの合計はそれぞれ128 MiBまでです。この上限を超える大判PSBには対応していません。
- 判定には合成画像の終端、レイヤーとマスクのチャンネル長・行幅・展開長、ZIPのチェックサムを使います。非圧縮・PackBitsには画素のチェックサムがなく、正規の長さを保った画素の変更を検出できない場合があります。効果・スマートオブジェクト等の付加情報は、境界を確認して元のバイトを保持します。
- 内容検索だけでは元の名前・保存場所・削除の有無を判定できません。断片が分離した文書、上書き済みの画素、読めなかった領域の内容はこの機能から再構築できません。
