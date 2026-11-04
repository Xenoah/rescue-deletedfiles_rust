# 実装目標と進捗

ユーザーの指示: 仕様に沿ってフェーズごとに自律的に実装・検証・push・リリースする。
対象と最優先指標は[SPEC.md](SPEC.md)を参照する。
このファイルは工程の記録であり、バックグラウンドの自動実行予約ではない。

| フェーズ | 完了条件 | 状態 |
| --- | --- | --- |
| 仕様 | 対象全カテゴリ、GUI/CLI/MCP、比較・受入基準 | mainに公開済み |
| v0.1 alpha | NTFS RAWの基本復元、GUI/CLI/MCP、正解付きテスト、Windows成果物 | v0.1.0-alpha.2公開済み。Windows/Linux CI成功、公開ZIPのSHA-256一致 |
| v0.2 alpha | FAT32/exFAT、圧縮/ADS、主要形式のcarving | 圧縮/ADS・FAT32/exFATの基本復元をv0.2.0-alpha.3で公開済み。Windows/Linux CI・公開ZIPのSHA-256一致を確認。PNG carving・根拠を示すGUI・技術説明UIをv0.2.0-alpha.4で公開済み。Windows/Linux CI・全配布物のSHA-256一致を確認。JPEG等は未完了 |
| v0.3 alpha | 実デバイス、読取マップ、イメージ取得、論理破損 | 未着手 |
| v0.4 alpha | 対応RAIDの仮想再構成 | 未着手 |
| v0.9 beta | 統合、異常系、大容量、Windows操作性 | 未着手 |
| v1.0 | 全必須範囲の受入と有料製品比較の公表 | 未着手 |

各フェーズで変更の実装、テスト、文書更新、コミット、push、リリースの順に進める。
テスト未実施やWindows未確認を検証済みと表現しない。
外部のアクセス制限がある場合は証拠を記録し、独立して進められる作業を続ける。
全フェーズ完成前にv1.0を付けない。プレリリースは対応範囲を明記する。

v0.1のローカル結果: Rust 9テスト、CLI/MCP 24項目（Windowsは23項目）、ntfs-3g由来メタデータの復元、
Linux/Xvfb GUIのスキャン・選択・復元とバイト一致を確認済み。
Windows debug/releaseのCLI/MCP実行は[公開CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41660787133)で成功。
ライセンスはユーザー指定のMIT。

第2フェーズ内の順序: (1) NTFS圧縮/ADS → (2) FAT32/exFAT → (3) 主要形式のcarving。
各小区切りでも検証後にタグを追加し、既存タグは付け替えない。
v0.2.0-alpha.1は[CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41664246239)成功、
公開ZIPのSHA-256一致を確認済み。alpha.3ではFAT32/exFATの13ファイルと異常系・取消17項目を検証。
最新の[Windows版リリース](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.5)と
[成功したCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41812660512)を確認済み。
次はJPEG/PDF/ZIP/MP4等の主要形式へcarvingを拡張する。FATのチェーン喪失時の推測や壊れたブート領域の探索は未完了。
この環境には/goalの登録・継続実行ツールがないため、会話終了後の自動実行は登録していない。

README・リリースには毎回、実際の画面、技術的根拠、検証環境・制限を掲載する。
操作可能な技術説明UIはdocs/intelligence.htmlを更新し、ZIPと単体HTMLで公開する。

v0.2.0-alpha.4はPNG 23項目・23ファイルの正解一致、実GUIの3ファイル照合を確認。
公開ZIP（12ファイル）、単体HTML、2枚のスクリーンショットを取得して全チェックサムを検証済み。
技術説明HTMLは公開ファイルでも両方式の切替・画像表示・外部通信なしを確認。

2026-11-04の実装監査で8件の不具合を確認。v0.2.0-alpha.5で全件を修正・公開済み。Windows/Linux CI成功、公開成果物5点のSHA-256一致を確認した。
監査条件の回帰16項目、PNGの強化30項目、GUI状態・表示の3テストを追加。
詳細は[AUDIT-FIXES.md](AUDIT-FIXES.md)。次の形式追加より監査修正を優先する。

alpha.5は[タグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41812660512)が4分46秒で成功。
ZIP内14ファイル、x64 PE、監査修正記録、実画面、単体HTMLの動作を検証済み。


2026-11-04：ユーザーから表示と配布要件を追加。alpha.6では起動時の保存／詳細検索設定、Explorer風の詳細列・並べ替え・グループ化、アイコン表示、実ZIP保存を実装した。
Windows x64、Linux x64、macOS Intel/Apple Siliconの4ネイティブビルドと、全OSのインストール／ポータブル配布を必須に変更した。
ローカルはRust 25項目、既存5群、追加の検索・日時・ZIP統合60項目、Linux全形式・独立FS検証、GUIからの36ファイルZIP照合を通過。
各OSのCI・インストーラー検証、公開成果物の取得確認を経てalpha.6を公開する。実装済みと公開済みは区別して記録する。
新しい画面とSHA-256付きのGUI検証は[UI-FLOW.md](UI-FLOW.md)。従来の監査修正と形式別試験を継続する。
