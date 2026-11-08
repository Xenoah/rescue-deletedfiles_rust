# Rescue

Rust製のファイル復元ソフト。Windows・Linux・macOSのGUI・CLI・MCPが共通エンジンを使います。復元できるファイルの数と、元の内容との一致を重視しています。

**v1.1.0**ではUIを整理し、FAT12/16、明示的なFAT推定復元、HTML/CSVレポートを追加しました。

[リリース・ダウンロード](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.1.0) · [変更内容](docs/RELEASE-v1.1.md) · [インストール](docs/INSTALL.md) · [検証台帳](docs/V1_1_ACCEPTANCE.md)

![v1.1の設定画面](docs/images/v1.1/settings-ja.png)

## ダウンロード

通常はインストーラーを1つ選んでください。インストールせず使う場合はポータブル版を選べます。

| 環境 | インストーラー | ポータブル |
| --- | --- | --- |
| Windows x64 | [セットアップEXE](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-windows-x64-setup.exe) | [ZIP](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-windows-x64-portable.zip) |
| Linux x64 | [DEB](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-linux-x64.deb) | [tar.gz](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-linux-x64-portable.tar.gz) |
| macOS Apple Silicon | [PKG](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-macos-arm64.pkg) | [ZIP](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-macos-arm64-portable.zip) |
| macOS Intel | [PKG](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-macos-x64.pkg) | [ZIP](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/rescue-1.1.0-macos-x64-portable.zip) |

リリース添付は8配布物と[共通SHA-256](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v1.1.0/SHA256SUMS.txt)の9点です。画面・コーパス・検証JSONはリポジトリとActionsに保存します。GitHubの自動生成ソースコードは別枠です。

Windowsは発行元署名なし、macOSはアドホック署名・公証なし。Linuxはglibc 2.35以降、macOSは13以降です。[起動条件](docs/INSTALL.md)を確認してください。

## できること

- NTFS・FAT12/16/32・exFATの削除管理情報を解析。MBR/EBR/GPT、長い名前、対応する断片化チェーンを扱います。
- 入力と保存先をカードで分離し、現在のステップ、選択件数、推定・不完全な結果を表示。狭い画面では縦配置になります。
- 取消・再開可能な索引、検索、隔離プレビュー、フォルダー/ZIP/元の場所への復元。
- 読取マップ付きイメージ取得、消失領域探索、残存MFT、対応するRAID構成、PNG/JPEG/PDF/ZIP/Office/MP4/MOVのCarving。
- 復元結果のHTML/CSV保存。元のJSONと同じ警告・欠損・名前・出力ハッシュを保持します。

![保存完了と推定を区別](docs/images/v1.1/recovered-ja.png)

FATのチェーンが消えた複数クラスターは標準では推定しません。詳細設定で有効にした場合だけ連続配置を推定し、割当競合や構造不正を拒否します。同じ固定FAT32イメージでは標準14/36件、推定有効36/36件が元のSHA-256と一致しました。一般的な復元率ではありません。未知形式の断片化では誤った推定があり得るため、保存後も「内容未確認」と表示します。[対応範囲と実測](docs/FAT-v1.1.md)。

![ブラウザーで読む復元レポート](docs/images/v1.1/html-report.png)

[UIの実操作](docs/UI-v1.1.md) · [レポート](docs/REPORTS.md) · [現行の制限](docs/ALPHA.md) · [索引](docs/INDEX.md) · [同じ場所への保存](docs/SAME-LOCATION.md)

APFS/ext4/ReFSの直接解析、実SSD/HDDでの一般的な復元率、第三者スクリーンリーダーによる人の評価は未対応・未評価です。読み取りは元媒体を変更しませんが、復元先は別媒体を推奨します。推定・保存完了・出力ハッシュは削除前との内容一致を証明しません。

## CLI / MCP

```sh
rescue scan --source disk.raw --indexed --work-dir work --json
rescue results --session work/SESSION.rescue-index/session.sqlite --json
rescue recover --session work/SESSION.rescue-index/session.sqlite --all --destination recovered --json
rescue export-report --input recovered/rescue-report-ID.json --format html --output report.html --json
rescue mcp --stdio --allow-source disk.raw --work-dir work --allow-output-root recovered
```

MCPは許可した入力と出力範囲を固定し、ジョブの進行・取消・再開と候補検索・復元を提供します。詳細は `rescue --help`、[仕様](docs/SPEC.md)、[クラウドセットアップ](docs/CLOUD-TESTS.md)を参照してください。

## 開発と検証

Rust 1.99.1、MITライセンス。4ネイティブ環境の全検証に成功してからリリースを公開し、公開した実物も再ダウンロードしてテストします。

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python scripts/run_integration.py --binary target/debug/rescue
```

現行は30統合群です。WindowsのUIA・実キー入力・Microsoft日本語IMEは通常権限のGUIで別途検証します。検証内容と限界は[受入台帳](docs/V1_1_ACCEPTANCE.md)に記録します。

過去の記録: [v1.0リリース](docs/RELEASE-v1.0.md) · [v1.0受入](docs/V1_ACCEPTANCE.md) · [1 TiB・1,000万候補の実測](docs/PERFORMANCE-v1.md) · [商用製品との同一入力比較](docs/COMPARISON-v1.md) · [v1.1以降のバックログ](docs/BACKLOG_V1_1.md)。過去の画像や計測を現行版の実績として数えません。
