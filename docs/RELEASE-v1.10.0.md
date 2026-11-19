# Rescue v1.10.0

固定容量・可変容量のVHDを、GUIとCLIで直接RAWディスクに変換できるようになりました。削除管理情報を失った媒体では、AIFF・AIFF-Cの音声、TrueType・OpenType・WOFFのフォントを内容から検索できます。

## VHDディスクの取り込み

GUIの「ディスクのツール」から「イメージをRAWに変換」を開き、VHDと新しいRAWの保存先を指定します。完了したRAWがスキャン元になります。CLIでは次のように実行します。

```sh
rescue import-image --source backup.vhd --output disk.raw --json
```

対象は親を持たないクラシックVHDです。固定容量型は末尾のフッターと記録容量を、可変容量型は前後のフッター、動的ヘッダーのチェックサム、ブロックの位置・重複、セクターの割り当てを確認します。未割り当てセクターはゼロとして読み取ります。VHDXの取り込みには引き続きThe Sleuth Kitを使用します。[イメージの取り込み](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.10.0/docs/IMAGING.md)

## 音声とフォントの内容検索

```sh
rescue scan --source disk.raw --mode carve --extension aiff --extension ttf --extension otf --extension woff --work-dir scan --json
```

| 形式 | 照合する内容 |
| --- | --- |
| AIFF・AIFF-C | FORMと各チャンクの境界、チャンネル数、サンプル数・符号化、音声データ長。整数PCM、32/64ビット浮動小数点、A-law・μ-lawに対応 |
| TrueType・OpenType | テーブル配置、テーブル単位とファイル全体のチェックサム、文字マップ、水平メトリクス、グリフ輪郭テーブル |
| WOFF 1 | 圧縮テーブルの展開、元の長さとチェックサム、付属メタデータとプライベートデータの範囲、復元したフォント構造 |

`.aif`・`.aiff` はAIFFとAIFF-Cを、`.aifc`・`.afc` はAIFF-Cを検索します。`.otf` はOpenTypeフォントとOpenDocument数式テンプレートの両方で使われます。内容を確認して区別し、以前の数式テンプレートの保存セッションも読み取れます。対象は連続配置され、形式内の長さを検証できる64 MiBまでのファイルです。フォントコレクション、WOFF 2、QuickTime IMA ADPCM形式のAIFF-Cは対象外です。

## 実証結果

- FAT32で7ファイルを削除した68 MiBのディスクをQEMUで固定・可変の2種類のVHDに変換しました。GUIで取り込んだRAWとQEMUで独立に展開したRAWは、いずれも元ディスク全体のSHA-256と一致しました。両ディスクから削除前のPNG・WebP・ICO・テキスト・XML・ZIP・bzip2を復元し、計14件のSHA-256が一致しました。CLIのビットマップ試験では、未割り当ての物理セクターに意図的に残したデータが復元結果に混入しないことを確認しました。[VHD試験](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.10.0/scripts/e2e_vhd.py)
- 音声10原本は通常・索引の2経路で20件すべて一致し、FFmpegで開いて復号したPCMも照合しました。15件の破損入力を拒否しました。[音声の原本ハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.10.0/tests/aiff/manifest.json)
- 独立に作成したフォント10原本とLinuxにインストールされた5書体は、2経路で30件すべて元のバイト列と一致しました。fontToolsで開き、FreeTypeで文字を描画しました。配置・チェックサムなどが壊れた19件を拒否しました。[フォントの原本・生成コード](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/v1.10.0/tests/fonts)
- ext4で5形式のファイルを各2件書き込み、10件すべてを実際に削除しました。通常と索引の内容検索で20件すべてのバイト列とSHA-256が削除前に一致しました。[実削除の再現テスト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.10.0/scripts/e2e_deleted_audio_fonts.py)
- Linuxの実GUIで音声3点・フォント3点を選択し、ZIPの6ファイルとHTML・CSVレポートを照合しました。[GUIの試験記録と画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.10.0/docs/gui-v1.10-results.json)

35形式・36原本の既存試験では、正常なNTFS・FAT32・exFAT管理情報を使った8条件のCLI/MCPで576/576件が一致しました。NTFSの管理情報を消した場合は連続配置で31/36、断片化配置で11/36でした。元データを上書きした条件では一致は0件です。
