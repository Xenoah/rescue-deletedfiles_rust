# v1.2 受入台帳

2026-11-08 UTC。ユーザーの進行指示を受けた、破損USB/SDへの対応強化と既知ハッシュ照合を実装・公開・検証した。APFS等の別フェーズは含めない。

## 公開状態

4段階すべてを個別にpush・リリースし、各タグでWindows x64・Linux x64・macOS Intel/Apple SiliconのネイティブCIと公開実物の再検証に合格した。各リリースの添付は8インストーラー/ポータブルとSHA256SUMSの9点。既存タグや配布済みバイナリは変更していない。

| 公開版 | 範囲 | ネイティブCI | 公開実物 | 保存した証拠 |
| --- | --- | --- | --- | --- |
| [v1.2.0-alpha.1](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0-alpha.1) | 段階的な取得・待機上限・再接続 | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42471633276) | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42474088556) | [全記録](verification/v1.2.0-alpha.1-published.json) · [公開確認](verification/v1.2.0-alpha.1-release.json) |
| [v1.2.0-alpha.2](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0-alpha.2) | CP932短名・元バイト保持 | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42474565163) | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42477620895) | [全記録](verification/v1.2.0-alpha.2-published.json) · [公開確認](verification/v1.2.0-alpha.2-release.json) |
| [v1.2.0-alpha.3](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0-alpha.3) | exFAT破損変換表の限定的継続 | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42475953647) | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42478332375) | [全記録](verification/v1.2.0-alpha.3-published.json) · [公開確認](verification/v1.2.0-alpha.3-release.json) |
| [v1.2.0](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0) | 既知SHA-256照合・統合・インストーラー版番号 | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42477916390) | [4環境合格](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42480695687) | [全記録](verification/v1.2.0-published.json) · [公開確認](verification/v1.2.0-release.json) |

公開後のJSONは署名・梱包後の実CLIを再ダウンロードして得た全記録。ActionsアーティファクトZIP内の全メンバーをPUBLISHED-SHA256SUMSで照合し、集約JSONをそのまま保存した。ZIPとJSONのSHA-256、公開9資産、タグのコミット、両ワークフローを公開確認JSONに記録している。

## 安定版の検証

- Rust 82テスト、35統合群、Clippy（警告をエラーとして扱う）、fmt、配布添付/版番号の3契約テスト。
- 段階的取得の13条件：実読取ヘルパー停止による期限超過、遅い結果の排除、取消、同一ソース再接続、差し替え拒否、旧マップ互換、逆方向再試行と全バイト一致。
- CP932の34条件・168出力：FAT12/16/32、LFN優先、削除先頭/曖昧境界/不正バイト、JSON/索引/MCP/ZIP/レポート、MBR/GPT、実プロセス強制終了後の再開。
- exFATの31条件・72出力：読取可能な変換表の内容/チェックサム破損に限った明示的継続、元の警告保持、配置/割当/エントリ検証、正常な表の名前ハッシュ検証、再開。
- 既知ハッシュの18条件・32出力：実CLI/MCPのフォルダー/ZIP復元、同じ内容の複数ラベル、一覧の書式/上限/矛盾、完了していない復元の区別、元の警告の保持、別JSONへの非上書き保存。
- Linuxローカル配布物の内容と同梱CLI/MCP 60項目に合格。ネイティブCIでは4環境のインストーラー/ポータブルと内部版番号を検証した。
- 公開実物は各環境で35形式・36ファイルの8条件×CLI/MCPが576/576、MBR/GPT/所有仮想ディスクの出力が864/864バイト一致。Windowsは通常権限のUIA・実キー・Microsoft日本語IMEも通過した。詳細と他の回帰条件は上表の全記録を参照。

[安定版のローカル集計](verification/v1.2.0-local.json) · [取得](acquisition-v1.2.json) · [CP932](cp932-v1.2.json) · [exFAT](exfat-damage-v1.2.json) · [既知ハッシュ](known-hashes-v1.2.json)

## 実GUIと互換性

安定版の実GUIをマウス/キーボードで操作し、26画面を撮影した。従来の36ファイル・152,325バイト、CP932の8ファイル・224バイト、破損exFATの8ファイル・14,135バイトを元データと照合した。exFATの全11候補で名前検証の欠落を保持し、配置を証明できない3件は復元対象から除外した。

取得GUIでは1秒の期限超過後に失敗/未読範囲を保持し、元ソースから再開して64 MiB全量のSHA-256が一致した。ハッシュGUIでは一致2件・一覧にない6件・照合不能0件を確認し、元の警告を含む別JSON保存、日英表示、720×520でのスクロールを検証した。

[実画面](UI-v1.2.md) · [画面/バイナリ/出力のSHA-256とGUI全記録](gui-v1.2-results.json)

CP932を使うセッションはスキーマ10、破損exFAT設定は11。旧版CLIの実物が新スキーマを拒否することを確認した。[alpha.2互換性](verification/v1.2.0-alpha.2-compatibility.json) · [alpha.3互換性](verification/v1.2.0-alpha.3-compatibility.json)。従来のASCIIセッションと旧linear読取マップは維持する。

先行版の画面/ローカル結果は各タグで固定して保存している。上表の公開後記録はそのタグの配布物を検証したもので、先行版の画像を安定版の画像として数えていない。

## 確認した範囲と残る課題

取得障害/破損表は所有する合成イメージと実プロセス停止で試験した。ネイティブの仮想ディスク・容量不足・アクセス拒否も検証したが、物理故障媒体の一般的な復旧率を測定したものではない。OS内I/Oや最初のデバイス検出の即時取消は保証しない。

exFATのビットマップ欠損、変換表の欠落/重複/不正チェーン、エントリ集合やUTF-16の不正は継続対象ではない。既知ハッシュは復元時に記録した値を比較し、保存後のファイルを再読取しない。名前の確定や一覧/外部レポートの真正性確認とは区別する。

追加FS・入力形式、断片化動画、カメラRAW、物理媒体での測定、署名/公証、人によるアクセシビリティ評価等は[後続課題台帳](BACKLOG_V1_1.md)に残す。

alpha.3の公開Windows試験は初回に日本語IME取消で失敗した。同一配布バイナリの別ホスト診断を通し、失敗したWindows公開ゲート全体を変更せず再実行して合格した。原因の断定や修正済みとの主張はしない。[失敗内容と診断記録](verification/v1.2.0-alpha.3-windows-diagnostic.json) · [実IMEの全結果](verification/v1.2.0-alpha.3-windows-ime.json)。安定版自身の公開検証は上表の別記録で確認する。
