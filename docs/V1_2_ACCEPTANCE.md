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

## v1.2.1: Windows IME取消の修正

EscがIME取消より先にフォーカスを外す経路を実eguiフレームで再現し、変換中のEscを入力欄で処理するよう修正した。既存文字の保持、同一フレームの両順序、別フレーム、キーリピート、別入力欄、通常のEscを検証する。旧処理の回帰テスト失敗と修正後の成功をWindowsで確認した。

[修正PR #4](https://github.com/Xenoah/rescue-deletedfiles_rust/pull/4)は[4環境CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42489397435)に合格し、[実IMEの8回連続試験](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42489393103)も再試行なしで成功した。6回目は最初のEscで取消完了し、2回目でフォーカスが外れたため、取消結果を検証してから次の変換に向けてフォーカスを戻す試験手順も確認できた。候補版の表示バージョンは1.2.0であり、v1.2.1配布物の証跡とは区別する。[ログ・画面・SHA-256](verification/v1.2.1-ime-candidate.json)。

v1.2.1のネイティブCIと公開実物の両方に8回連続試験を要求し、中間回の失敗や不足した証跡を拒否する。各回のPNGはActionsの専用アーティファクトに保持し、リリース添付は9点を維持する。

v1.2.1ローカルではRust 85テストと4契約テストが成功。GUIの26画面を新規撮影し、断片化NTFSの36ファイル、CP932の8ファイル、破損exFATの8ファイルを全バイト照合した。64 MiBの取得タイムアウト・再開、HTML/CSVの結果保持、既知ハッシュの2/6/0件・別JSON保存、日英切替・狭幅スクロールも確認した。[今回のGUI全記録](gui-v1.2-results.json)。従来のv1.2.0 GUI記録は[元タグ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.2.0/docs/gui-v1.2-results.json)に固定している。

過去の断続的失敗の正確な入力イベント列は採取していないため、単一原因と断定しない。今回の根拠は、旧処理で再現する取消経路の修正と実Microsoft日本語IMEの連続検証である。v1.2.1の公開完了と配布実物の結果は公開後に別記録へ追記する。

## v1.2.1の公開停止

v1.2.1タグの全体検証はLinux・Intel Mac・Apple Silicon Macに合格したが、Windowsの実IME試験で1回目の取消が失敗した。Escを2回送った後も「とりけし」が残り、フォーカスは両回とも検索欄だった。フォーカスを維持する修正だけでは残る経路を解消できていない。リリースは公開しておらず、同じ試験の再実行で成功扱いにはしていない。[失敗時の全記録](verification/v1.2.1-withheld.json)。

既存のv1.2.1タグは保持し、追加診断では実入力イベント・選択範囲・失敗画面を採取する。最新版のダウンロード案内は公開済みv1.2.0へ戻した。

追加診断では、同じWindows上でアプリを起動し直す2回目の初回取消で失敗を再現した。直前の未確定文字の描画以降、Escや入力言語の復元要求のイベントが記録されず、復元要求もタイムアウトした。停止箇所はまだ特定していない。[生イベント・失敗画面・SHA-256](verification/v1.2.1-native-stall/manifest.json)を保存し、所有するGUIプロセスのスレッド情報を追加採取している。
