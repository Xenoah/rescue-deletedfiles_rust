# 実装目標と進捗

ユーザーの指示: 仕様に沿ってフェーズごとに自律的に実装・検証・push・リリースする。
対象と最優先指標は[SPEC.md](SPEC.md)を参照する。
このファイルは工程の記録であり、バックグラウンドの自動実行予約ではない。

| フェーズ | 完了条件 | 状態 |
| --- | --- | --- |
| 仕様 | 対象全カテゴリ、GUI/CLI/MCP、比較・受入基準 | mainに公開済み |
| v0.1 alpha | NTFS RAWの基本復元、GUI/CLI/MCP、正解付きテスト、Windows成果物 | v0.1.0-alpha.2公開済み。Windows/Linux CI成功、公開ZIPのSHA-256一致 |
| v0.2 alpha | FAT32/exFAT、圧縮/ADS、主要形式のcarving | 圧縮/ADSはv0.2.0-alpha.1で公開・Windows/Linux CI成功。FAT32/exFATの基本復元を実装・Linux検証済み。割当競合の追加検証を含むalpha.3のWindows CIと公開を確認中。carvingは未着手 |
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
次の実装対象は主要形式のcarving。FATのチェーン喪失時の推測や壊れたブート領域の探索は未完了。
この環境には/goalの登録・継続実行ツールがないため、会話終了後の自動実行は登録していない。
