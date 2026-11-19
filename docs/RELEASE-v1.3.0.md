# Rescue v1.3.0

削除したファイルを探す方法を、残っている手掛かりに合わせて選べます。

| 手掛かり | 操作 | 対応範囲 |
| --- | --- | --- |
| 削除管理情報が残る | GUI・CLI・MCPの通常スキャン | NTFS、FAT12/16/32、exFAT。内容形式や拡張子を問わず、配置を確認できたファイルを復元 |
| その他の対応ファイルシステムの管理情報が残る | CLIの `filesystem` | ローカルのThe Sleuth Kitが読めるイメージの削除済み通常ファイル。ext2/3/4で削除・復元を検証 |
| ファイルが連続して残る | GUI・CLI・MCPのCarving | 従来のPNG/JPEG/PDF/ZIP/Office/MP4/MOVに加え、GIF、BMP、静止WebP、WAV、SQLite |
| 独自形式の境界が分かる | Carvingの独自形式ルール | 先頭の署名と固定長、または最初に一致する終端署名で範囲を推定 |
| 仮想・フォレンジックイメージを使う | CLIの `import-image` | The Sleuth Kitが読める形式を新規RAWに展開し、GUI・CLI・MCPで再利用。VHDとVMDKで論理ディスク全体を照合 |

## 利用例

`filesystem` と `import-image` にはThe Sleuth Kitの `fls`、`icat`、`img_cat` を別途インストールし、PATHに追加してください。利用できるドライバーは `fls -i list` と `fls -f list` で確認できます。先頭セグメント以降は `--segment` で順に指定できます。

```sh
rescue import-image --source disk.vmdk --image-type vmdk --output disk.raw --json
rescue filesystem --source disk.raw --sector-offset 2048 --fs-type ext4 --json
rescue filesystem --source disk.raw --sector-offset 2048 --fs-type ext4 --all --destination recovered --json
```

ボリューム単体のイメージでは `--sector-offset` を省略します。オフセットの単位は論理セクターです。個別選択には一覧に表示された `--id` を使います。出力名はメタデータIDと拡張子から作り、元のパスをJSONに残します。既存ファイルは保護され、読取エラーや長さの不一致があった出力は保存されません。OSが読取を許可した物理ディスク・ボリュームも、単一のRAW入力として指定できます。外部リーダーで物理媒体を扱う場合、出力先は別ディスクであることを確認します。Rescueの読取マップ付きRAWとRAIDマニフェストは共通エンジンで扱います。

独自形式の例では、`VEND` から最初の `END!` までを1ファイルとして探します。固定長128バイトなら `vendor:56454e44:size=128` を使います。

```sh
rescue scan --source disk.raw --mode carve --signature-rule vendor:56454e44:end=454e4421 --indexed --work-dir work --json
```

GUIでは詳細設定のCarvingを選び「独自形式のルール」に入力します。MCPでは `start_scan.signature_rule` に同じ文字列を渡します。先頭署名は4～16バイト、終端署名は2～16バイト、1候補の上限は64 MiBです。指定時はそのルールだけを探索します。復元時にもルール・バイト範囲・SHA-256を再照合します。

## 実証結果

| Linuxで実行した試験 | 結果 |
| --- | --- |
| 管理情報のないRAW上のGIF・BMP・WebP・WAV・SQLite | 通常と索引の各5件、計10件が全バイト・SHA-256一致。末尾1バイトを欠く各5形式は復元不可と判定 |
| ext2・ext3・ext4で7ファイルを作成後、debugfsで削除 | 各7件を復元。TXTと拡張子のない独自バイナリを含む |
| 削除済みext4をVHD、VMDK、2分割RAW、開始セクター指定の4条件で復元 | 各7件一致。上の3条件を含めて計49件。元イメージは不変 |
| 読み取り専用のLinuxループデバイスとして同じext4を接続 | 7件一致。上の7条件と合わせて8条件・56件すべてが元データと一致 |
| QEMU製VHD・VMDKからRAWへの展開 | 両方とも元の16 MiBの論理ディスクと全バイト一致 |
| 独自形式 | 固定長・終端ルールを通常・索引で照合。MCPでも復元。範囲外の終端とルールを書き換えたセッションを拒否 |
| GUIで設定から新規スキャンし、候補を選択してZIPへ保存 | 追加の5形式と独自形式1件が元ファイルと一致。HTML/CSV、CP932、破損exFAT、読取期限後の再開も検証 |

再実行用スクリプトは `scripts/e2e_file_structures.py`、`scripts/e2e_signature_rule.py`、`scripts/e2e_extended_filesystems.py` です。それぞれ `--binary` で検証対象のCLIを指定します。[GUI出力・ハッシュ・画面の検証記録](gui-v1.3-results.json)

## 復元できる条件

追加の構造検証は、GIF87a/89a、非圧縮BITMAPINFOHEADERのBMP、RIFFの静止WebPとPCM/IEEE float WAV、信頼できるページ数を持つSQLite 3の主データベースを対象とします。SQLiteのWALやジャーナルの再生、画像や音声の全デコードは行いません。カービングの境界は連続配置を前提とし、元の名前・削除状態・断片の順序は特定できません。終端署名がペイロード中に現れる独自形式では途中までを検出することがあります。

暗号化された媒体は復号済みの論理イメージが必要です。上書き・TRIMで失われたバイトや、位置・境界を特定する手掛かりがすべて消えた任意データの完全復元は保証できません。実SSD/HDD全機種での復元率は未測定です。


## 公開パッケージの検証

Windows x64、Linux x64、macOS Intel・Apple Siliconのインストーラーとポータブル版を公開しました。配布8点のSHA-256はすべて添付の一覧と一致しました。公開Linuxポータブル版から取り出したCLIでも、追加5形式、独自形式、ext系ファイルシステムと仮想ディスクの各試験を再実行しました。[4環境のビルドとネイティブ試験](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44527290297) · [公開後の4環境再検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44533320582)
