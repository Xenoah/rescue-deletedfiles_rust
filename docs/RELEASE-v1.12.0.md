# Rescue v1.12.0

CD-ROMのデータトラックを論理ISOに変換できます。複数のセッションを含むISOでは、追記時に消えたファイルを古いセッションから取り出せます。

## CD-ROMのBINイメージ

GUIで「ディスクのツール」から「イメージをRAWに変換」を開き、BINファイルと保存先を選びます。完了すると生成したISOがスキャン元になります。CLIでは次のように実行します。

```sh
rescue import-image --source track.bin --image-type cd-raw --output data.iso --json
rescue scan --source data.iso --mode carve --work-dir scan --json
```

Mode 1とMode 2 XA Form 1を内蔵リーダーで読み取れます。2352バイトのセクターと、96バイトのサブチャンネルを伴う2448バイトのセクターから、2048バイトずつユーザーデータを取り出します。全セクターで時刻アドレスの連続性、EDC、P/Qパリティを確認します。欠損・不一致があれば不完全なISOを確定しません。

## 過去のセッションからファイルを復元する

```sh
rescue iso-sessions --source data.iso --json
rescue iso-sessions --source data.iso --session 1 --json
rescue iso-sessions --source data.iso --session 1 --all --destination recovered --json
```

この操作には `xorriso` が必要です。まずセッションを一覧にし、古いセッションの番号を指定して候補を表示します。個別に取り出すときは `--all` を `--path '/Removed/example.bin'` に置き換えます。過去には通常ファイルがあり、現在のセッションに同じパスがない場合に復元できます。ファイル形式の判定は不要で、空ファイルやzisofsで圧縮されたファイルも対象になります。元のパスはレポートに記録し、安全な名前で新しいファイルに保存します。

## 実証結果

| 試験 | 原本と照合した結果 |
| --- | --- |
| CD-ROMトラックの取り込み | xorrisoでJPEG・PNG・TIFF・FLAC・RTF・XML・ZIPを記録し、次のセッションで7件を削除。cdrdaoで作った2種類の符号化を2352／2448バイトの両方で読み、4条件すべてでISO全体のSHA-256と削除7件のバイト列・SHA-256が一致 |
| 独立したリーダーでの確認 | bchunkでMode 1とXA Form 1を展開し、生成したISOがRescueの出力とバイト単位で一致。xorrisoで最新ルートからの消失を確認し、最初のセッションから7原本を照合 |
| 過去セッションの管理情報 | 別途作ったISOの第2セッションで削除した11件を、第1セッションから復元。独自形式、空ファイル、zisofs、引用符・改行・日本語の名前を含め、全件のバイト列・SHA-256が一致。現在も存在するパスと、同じ名前のディレクトリに置き換えられた項目を除外 |
| 破損したトラック | EDC・P/Qの破損、アドレス不連続、セクターの欠落、XAサブヘッダーの不一致など18条件で出力を拒否 |

[試験用のトラックと原本ハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/v1.12.0/tests/optical) · [セッション復元の再現テスト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.12.0/scripts/e2e_iso_sessions.py)

BINからの変換で対応するのは単一のデータトラックです。CD-DA音声、XA Form 2、混在トラック、スクランブルされたセクター、欠損セクターの訂正は対象外です。複数トラックを含むCUE/BINは、対象データトラックを事前に切り出してください。セッション復元には、過去のセッション表と実データがISOに残っている必要があります。名前変更や移動も候補に含まれます。

Linuxの実GUIで4種のトラックを取り込み、生成したISOがスキャン元に設定されたことを確認しました。各ISOの7ファイルを原本と照合したほか、GUIから選んで復元した7ファイルのZIPとHTML・CSVのレポートも確認しました。[GUIの検証記録と画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.12.0/docs/gui-v1.12-results.json)

35形式・36原本の既存試験では、管理情報を保持したNTFS・FAT32・exFATの8条件でCLI・MCPの576/576件が一致しました。管理情報が消えたNTFSの内容検索では、連続配置が31/36件、断片化配置が11/36件でした。元データを上書きした条件で一致した件数は0です。
