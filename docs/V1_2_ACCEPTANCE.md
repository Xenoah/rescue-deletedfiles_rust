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

## v1.2.0の検証

- Rust 82テスト、35統合群、Clippy（警告をエラーとして扱う）、fmt、配布添付/版番号の3契約テスト。
- 段階的取得の13条件：実読取ヘルパー停止による期限超過、遅い結果の排除、取消、同一ソース再接続、差し替え拒否、旧マップ互換、逆方向再試行と全バイト一致。
- CP932の34条件・168出力：FAT12/16/32、LFN優先、削除先頭/曖昧境界/不正バイト、JSON/索引/MCP/ZIP/レポート、MBR/GPT、実プロセス強制終了後の再開。
- exFATの31条件・72出力：読取可能な変換表の内容/チェックサム破損に限った明示的継続、元の警告保持、配置/割当/エントリ検証、正常な表の名前ハッシュ検証、再開。
- 既知ハッシュの18条件・32出力：実CLI/MCPのフォルダー/ZIP復元、同じ内容の複数ラベル、一覧の書式/上限/矛盾、完了していない復元の区別、元の警告の保持、別JSONへの非上書き保存。
- Linuxローカル配布物の内容と同梱CLI/MCP 60項目に合格。ネイティブCIでは4環境のインストーラー/ポータブルと内部版番号を検証した。
- 公開実物は各環境で35形式・36ファイルの8条件×CLI/MCPが576/576、MBR/GPT/所有仮想ディスクの出力が864/864バイト一致。Windowsは通常権限のUIA・実キー・Microsoft日本語IMEも通過した。詳細と他の回帰条件は上表の全記録を参照。

[安定版のローカル集計](verification/v1.2.0-local.json) · [取得](acquisition-v1.2.json) · [CP932](cp932-v1.2.json) · [exFAT](exfat-damage-v1.2.json) · [既知ハッシュ](known-hashes-v1.2.json)

## v1.2.0の実GUIと互換性

安定版の実GUIをマウス/キーボードで操作し、26画面を撮影した。従来の36ファイル・152,325バイト、CP932の8ファイル・224バイト、破損exFATの8ファイル・14,135バイトを元データと照合した。exFATの全11候補で名前検証の欠落を保持し、配置を証明できない3件は復元対象から除外した。

取得GUIでは1秒の期限超過後に失敗/未読範囲を保持し、元ソースから再開して64 MiB全量のSHA-256が一致した。ハッシュGUIでは一致2件・一覧にない6件・照合不能0件を確認し、元の警告を含む別JSON保存、日英表示、720×520でのスクロールを検証した。

[v1.2.0の実画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.2.0/docs/UI-v1.2.md) · [v1.2.0のGUI全記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.2.0/docs/gui-v1.2-results.json)

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

v1.2.1ローカルではRust 85テストと4契約テストが成功。GUIの26画面を新規撮影し、断片化NTFSの36ファイル、CP932の8ファイル、破損exFATの8ファイルを全バイト照合した。64 MiBの取得タイムアウト・再開、HTML/CSVの結果保持、既知ハッシュの2/6/0件・別JSON保存、日英切替・狭幅スクロールも確認した。[v1.2.1候補のGUI全記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.2.1/docs/gui-v1.2-results.json)。従来のv1.2.0 GUI記録は[元タグ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.2.0/docs/gui-v1.2-results.json)に固定している。

過去の断続的失敗の正確な入力イベント列は採取していないため、単一原因と断定しない。今回の根拠は、旧処理で再現する取消経路の修正と実Microsoft日本語IMEの連続検証である。v1.2.1の公開完了と配布実物の結果は公開後に別記録へ追記する。

## v1.2.1の公開停止

v1.2.1タグの全体検証はLinux・Intel Mac・Apple Silicon Macに合格したが、Windowsの実IME試験で1回目の取消が失敗した。Escを2回送った後も「とりけし」が残り、フォーカスは両回とも検索欄だった。この時点では、フォーカス維持の修正後も失敗する原因を特定できていなかった。リリースは公開しておらず、同じ試験の再実行で成功扱いにはしていない。[失敗時の全記録](verification/v1.2.1-withheld.json)。

既存のv1.2.1タグは保持し、追加診断では実入力イベント・選択範囲・失敗画面を採取する。最新版のダウンロード案内は公開済みv1.2.0へ戻した。

追加診断では、同じWindows上でアプリを起動し直す2回目の初回取消で失敗を再現した。直前の未確定文字の描画以降、Escや入力言語の復元要求のイベントが記録されず、復元要求もタイムアウトした。この時点では停止箇所を特定していなかった。[生イベント・失敗画面・SHA-256](verification/v1.2.1-native-stall/manifest.json)を保存し、所有するGUIプロセスのスレッド情報を追加採取している。

[所有GUIのネイティブスタック](verification/v1.2.1-native-stall/native-stack-manifest.json)では、メインスレッドはWindows日本語IMEの内部で待機し、予測変換のワーカーもRPC応答を待っていた。両Esc後とも前面ウィンドウは対象GUIで、WM_NULLには応答した。永続的なデッドロックか処理の遅延かは、この一時点の採取だけでは断定しない。

追加キー・再フォーカス・文字列消去を行わず失敗後の経過を観測すると、未確定文字の描画から5.587秒後（旧失敗判定後の観測3.551秒）に取消通知が届き、検索文字列が空になった。[完了までの実イベント・画面・記録](verification/v1.2.1-native-stall/native-wait-manifest.json)。固定約1.1秒の判定がWindows IMEの処理完了より先だった。修正ではEscを2回だけ送り、実UIA値の空文字への変化を最大10秒待つ。旧診断結果は失敗のまま保持し、修正後の配布用ビルドは別の試験で検証する。

## v1.2.2候補の修正と再検証

取消はEscを2回だけ送り、最大10秒で実UIA値が空になることを検証する。遅れて届いた実イベント列を再現するRust回帰試験は、修正後に合格し、v1.2.0の旧処理では確定済み文字に未確定文字が残って失敗する。

続く変換試験にも固定400ミリ秒の待機があり、Spaceの後に表示状態を確認せずEnterを送り、「げんぶん」のまま確定していた。取消の待機修正が有効だった[実行42500970262](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42500970262)で確認した。末尾をnnで明示するgenbunn入力に統一し、変換前の「げんぶん」、変換後の「原文」、確定後の選択範囲の縮退を順番に確認する。ただし、末尾nを完成させても2回目の新規起動で変換に失敗したため、これを原因解決とは扱わない。

ローカルのv1.2.2はRust 86テスト、35統合テスト群、Clippy（警告をエラーとして扱う）、fmt、配布・版番号・IMEの5契約テストに合格。新規撮影した26画面でNTFS 36件、CP932 8件、破損exFAT 8件の全バイト一致、64 MiBの取得タイムアウトと再開、HTML/CSV、既知ハッシュの2/6/0件を確認した。[実画面](UI-v1.2.md) · [GUI全記録](gui-v1.2-results.json)。Windowsの新しい状態確認経路は検証中で、v1.2.2は未公開。

状態確認を追加した初回試験では、テスト用PowerShellの.NET TextPattern.GetTextでAccessViolationExceptionが発生した。検証成功とは扱わず、Windows標準のCOM APIを直接使い、対象GUIのPIDと検索欄の名前を確認して選択文字・選択範囲を読む試験用クライアントを追加した。[変換タイミング失敗・読み取り失敗・旧処理の回帰ログ](verification/v1.2.2-ime-diagnostics/manifest.json)。このクライアントは製品や配布パッケージには含めない。

[実行42505810350](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42505810350)の生イベントでは、Spaceの後にWindowsが通知した文字列も「げんぶん」で、「原文」の通知はなかった。前面は対象GUIのままだった。失敗を保存した後の15秒の観測と、診断用の追加Space 1回でも変換しなかった。追加操作は合格判定に使用していない。

終了時に共有IMEサービスを巻き込む仮説も検証したが、[実行42506839758](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42506839758)は最初のIME起動で取消に失敗し、先行するGUI試験の終了時に残存プロセスはなかった。サービス保持処理は使用されておらず、この仮説は支持されないため変更を取り除いた。10秒の期限後さらに15秒観測しても未確定文字が残り、メインスレッドは通常のメッセージ待機中だった。標準WPF入力欄で同じ操作を行う比較診断を追加し、製品と環境の切り分けを続けている。全記録と実画面は[診断台帳](verification/v1.2.2-ime-diagnostics/manifest.json)に保持する。
