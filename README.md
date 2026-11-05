# Rescue

Rust製のファイル復元ソフト。Windows・Linux・macOSでGUI・CLI・MCPが同じ復元エンジンを使います。
復元できるファイルの数と、元データの内容を保つことを優先して開発しています。

**v0.2.0-alpha.10: PDF・ZIP/Office・MP4/MOVの探索と、画面幅に合わせたGUI配置を追加。**
現在は開発版です。v1.0の全必須機能・レビュー・3 OS試験は [受入台帳](docs/V1_ACCEPTANCE.md) で追跡します。
RAID、読取マップ付きイメージ取得、消失領域探索、大容量索引などは未完了です。
有料製品との性能比較は未測定です。

![名前・日時・種類・サイズ・状態を並べて表示する実GUI](docs/images/details-view.png)

列見出しで並べ替え、境界で列幅を変更し、元フォルダー・種類・日付・サイズ等でグループ化できます。
画像はLinuxの実GUIと人工生成のディスクです。日時や元のフォルダーは試験用の値です。

## ダウンロード

[v0.2.0-alpha.10のリリースとSHA-256一覧](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.10)

| OS / CPU | インストール版 | ポータブル版 |
| --- | --- | --- |
| Windows x64 | [setup.exe](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-windows-x64-setup.exe) | [portable.zip](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-windows-x64-portable.zip) |
| Linux x64 | [.deb](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-linux-x64.deb) | [portable.tar.gz](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-linux-x64-portable.tar.gz) |
| macOS Apple Silicon | [.pkg](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-macos-arm64.pkg) | [portable.zip](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-macos-arm64-portable.zip) |
| macOS Intel | [.pkg](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-macos-x64.pkg) | [portable.zip](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/rescue-0.2.0-alpha.10-macos-x64-portable.zip) |

Linuxはglibc 2.35以降、macOSは13以降。macOSはアドホック署名で、Developer ID署名・公証はありません。
OS対応とファイルシステムの解析対応は別です。APFS/ext4の直接解析は未対応です。

## 操作の流れ

1. ディスク全体・ボリューム、または保存したRAWイメージを選択。
2. 元の場所・任意フォルダー・ZIPと、同名ファイルの処理を設定。
3. 必要なら詳細設定で検索方式・範囲・深度・拡張子を指定してスキャン。
4. 復元するファイルを選択し、復元結果と保存先を確認。

![保存方法を先に設定する実画面](docs/images/settings-view.png)

同じディスクにも保存できます。選択データをすべて先に読み出します（合計512 MiBまで）。
同名ファイルは `資料 (復元 1).pdf` のように別名で残すか、スキップ。同名フォルダーは統合します。
元の場所が確認できない候補はフォルダーかZIPへ保存してください。
同じディスクへの保存は、未選択の削除データへ影響する可能性があります。

主要操作はスクロール領域の下に固定し、入力ラベルを常に表示します。
Ctrl+F（macOSはCommand+F）で結果検索へ移動。最小720×520と100%/150%/200%の配置を検査しています。
配置の判断根拠は [Microsoft/W3Cの公式資料と採用ルール](docs/UI_GUIDELINES.md) を参照。

## 復元の根拠

| 方式 | 確認する内容 | 限界 |
| --- | --- | --- |
| Quick | NTFSのMFT/runlist、FAT32/exFATの削除エントリーと配置 | 拡張子を問わずバイトを保存。一般の内容の形式検証は未実施 |
| PNG / JPEG | PNGのCRC・zlib・画素、JPEGの全MCU・係数・画素 | 連続配置、元名・日時・削除状態は不明 |
| PDF | classic xref・間接参照・ページ階層・streamの長さ | 描画未検証、EOFによる範囲推定、xref stream/暗号化は未対応 |
| ZIP / Office | 全エントリーのCRC・展開長・管理情報、OfficeのXML分類 | stored/DEFLATE。Office描画・マクロの安全性は未検証 |
| MP4 / MOV | moov/mdat・トラック・全サンプル位置 | 非fragmented、ftyp必須。映像/音声の全フレーム復号は未実施 |

![文書・Office・動画の7形式を選択した実GUI](docs/images/document-carving.png)

[文書・動画の具体的な対応と上限](docs/DOCUMENT-CARVING.md) · [JPEGの検証](docs/JPEG-CARVING.md) · [全対応表](docs/ALPHA.md)

[操作できる技術説明UIをダウンロード](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.10/intelligence.html)。画像を内蔵した単一HTMLで、通信せずに画面と探索方式を切り替えられます。

![各方式の検証根拠と制限を説明するインテリジェンスUI](docs/images/technical-ui-v10.png)

## 検証

- LinuxのRust 33項目、9統合群。新しい文書/動画の69ケースでは正常23変種、破損/未対応30条件、CLI/MCPの30出力のバイト一致を確認。
- 35形式36ファイルの管理情報復元は8条件×CLI/MCPで576件一致。管理情報を失った連続配置は11/36、512B単位の断片配置は2/36。全900観測中755件が正解一致。上書き済み36件は元データとの一致0件で、管理情報から保存できても内容の同一性を保証しないことも確認。
- Linux実GUIで、元の場所36件・ZIP36件・JPEG33件・文書/動画7件の出力、既存PDFの保持、入力不変を照合。
- Windows・Linux・macOS Intel/ARMの実VMで同じ試験を実行。各VM内にこの試験専用のVHD/loop/hdiutilディスクを作り、読取専用ディスクと実際のPDF削除・元の場所への復元を確認しました。

[4環境のネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41971886923)がすべて成功しました。[公開パッケージの4環境試験](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41974636495)も成功し、署名・梱包後の実CLI、公開Linux GUIの7形式復元、配布71資産の照合まで完了しました。[公開実物の検証記録](docs/public-release-v0.2.0-alpha.10.json) · [公開GUIの記録](docs/public-gui-v0.2.0-alpha.10.json) · [検証履歴](docs/VALIDATION.md)
GUIの自動配置試験・起動試験と、Windows/macOSの手動アクセシビリティ評価は区別します。
人工コーパスの件数は、実SSDのTRIM・上書き・物理故障や、一般的な復元率を示しません。

## CLI / MCP

```sh
rescue sources --json
rescue scan --source volume.raw --work-dir work --json
rescue scan --source disk.raw --mode carve --extension pdf,docx,mp4 --work-dir work --json
rescue results --session work/SESSION.rescue-session.json --json
rescue recover --session work/SESSION.rescue-session.json --all --destination recovered --json
rescue restore --source C: --name report.pdf --original --json
rescue mcp --stdio --allow-source volume.raw --work-dir work --allow-output-root recovered
```

MCPは許可した入力・出力先を固定し、長時間処理の開始・進行取得・取消と候補検索・復元を提供します。
詳細は `rescue --help`、[仕様](docs/SPEC.md)、[クラウドセットアップ](docs/CLOUD-TESTS.md) を参照。

## 開発

Rust 1.99.1を固定しています。MITライセンス。

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python scripts/run_integration.py --binary target/debug/rescue
```

v1.0は [受入台帳](docs/V1_ACCEPTANCE.md) の必須項目を満たしてから公開します。
未実装を既知の制限へ移して安定版と呼ぶことはしません。
