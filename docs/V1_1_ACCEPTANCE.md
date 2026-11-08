# v1.1.2 受入台帳

2026-11-08 JST。今回の範囲は、UI/配布整理 → FAT12/16と明示的推定 → HTML/CSVレポートの3段階です。alpha.1、alpha.2を個別にpush・タグ付けし、各段階でネイティブ検証を通して公開しました。`BACKLOG_V1_1.md` はv1.1以降の長期候補も含み、その全項目を今回の完成条件としたものではありません。

## 実装・ローカル検証

| 条件 | 状態・証拠 |
| --- | --- |
| 現在のステップ、入力と保存先、主操作、狭幅配置、形式別アイコン | 実装済み。[最終版の実画面](UI-v1.1.md) |
| 復元後に設定へ戻った際の不要なレポート余白 | 修正し720×520で確認 |
| 添付は利用者向け配布物8点＋共通SHA-256のみ | `release_assets.py`の明示リスト、欠落/余分なファイルの試験2件。alpha.1・alpha.2・v1.1.2の公開物で各9点を確認 |
| パッケージ内部を利用ガイド・ライセンス中心に整理 | 画像・試験コーパス・検証JSONの一括同梱を終了 |
| FAT12/16、固定ルート、LFN、断片化、FAT12境界、MBR/GPT、探索 | [20項目・60件のバイト一致](classic-fat-v1.1.json) |
| 明示的FAT推定、全有効FATの空き/所有権/構造/範囲検証、再開と復元時の再検証 | [20項目・10件一致](estimated-fat-v1.1.json)。未知形式の断片化1件は意図的な不一致として未確認表示を保持 |
| 独立したFAT12/16実削除 | mkfs.fat/mcopy/mdelで[8件一致](independent-classic-fat-v1.1.json) |
| v1.0と同じ入力のFAT32再評価 | [標準14/36・明示的推定36/36](fat32-comparison-v1.1.json)。一般的復元率とは扱わない |
| HTML/CSV全情報保持、文字列対処、上限、上書き拒否 | [14項目、実復元6件](reports-v1.1.json) |
| GUIの新規スキャン・検索・選択保持・ZIP復元 | [36件・152,325バイト一致](gui-v1.1-results.json)。推定22件 |
| GUIからHTML/CSVを実保存 | 全36結果がZIP内JSONと一致。ChromiumでHTMLを表示、外部要求0 |
| Rust/Clippy/統合群 | Rust 74、Clippy warnings禁止、31統合群がローカルで成功 |

## 公開条件

`.github/workflows/ci.yml`はWindows x64、Linux x64、macOS Intel、macOS ARMの4環境を必須とし、すべて成功した場合だけタグからリリースを生成します。既存のNTFS/Carving/索引/取得/ディスク/復元/RAID/変異入力の検証を継続し、FAT12/16・推定・レポートを追加しました。独立したFAT12/16実削除もLinuxで必須です。

Windowsでは通常権限のGUIをUIA、ネイティブキー、実Microsoft日本語IMEで操作し、日本語ファイルを復元します。Linuxの画像はこのWindows検証の代替ではありません。

公開後は`published-packages.yml`が同じ不変タグの試験コードを使用し、公開ポータブルを再取得してSHA-256をネイティブ検証アーティファクトと照合します。署名・梱包後の実行ファイルに対して同じ主要群と新しい4群、Windows GUIを再実行します。公開検証のリンクはリリースノートへ追記されます。

詳細資料はActionsの`native-*`と`published-verification`に90日保存します。画面・ローカル結果は本リポジトリに保持し、公開後の集計と全結果JSONも下記に保持しています。画像やJSONをリリース添付へ追加しません。

## 範囲と限界

推定は元内容の証明ではありません。構造検証を通過しても誤り得ます。標準設定では無効、1ファイル64 MiBまで。通常の保存は選択した合計512 MiBを先読みします。

APFS/ext4/ReFS直接解析、VHD/E01、追加RAW形式、実HDD/SSDでの広い測定、発行元証明書/公証、第三者スクリーンリーダーでの人の評価は今回追加していません。これらを測定済み・対応済みとは表記しません。既存の[1 TiB・1,000万件測定](PERFORMANCE-v1.md)と[商用製品比較](COMPARISON-v1.md)はv1.0時点の記録です。

## 段階リリースの確認

| 段階 | ネイティブCI | 公開実物4環境 | 添付 | 永続記録 |
| --- | --- | --- | --- | --- |
| v1.1.0-alpha.1 | 成功 | 成功 | 9点 | [公開結果](verification/v1.1.0-alpha.1.json) |
| v1.1.0-alpha.2 | 成功 | 成功 | 9点 | [公開結果](verification/v1.1.0-alpha.2.json) |
| v1.1.2（最新安定版） | 成功 | 成功 | 9点 | [公開結果](verification/v1.1.2.json) |

最終版は[ネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42438017936)と[公開パッケージ検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42442795449)がすべて成功しました。公開Windows GUIでも通常権限のUIA・実キー・実Microsoft日本語IMEの3経路が成功し、各2ファイルをバイト照合しています。

[公開実物の集計](verification/v1.1.2-public.md)と[全結果JSON](verification/v1.1.2-public-full.json)は、ActionsアーティファクトのZIPと内部チェックサムを確認して、そのまま保存しました。FAT12/16は各環境60件、推定は10件と意図的な不一致1件、レポートは14項目、Issue #1–#3は9項目・9出力を検証しています。リリース添付への画像・検証JSON追加はありません。

## 公開前に見つかったIssue #1–#3

`v1.1.0`の検証を公開前に取り消し、タグは保持しました。`v1.1.2`で長い名前のraw出力、バッチ途中の失敗結果、索引FATミラー警告の永続化、通常のコロンとADSの種類判定を修正しています。[9項目・9出力の再現試験](release-fixes-v1.1.json)をネイティブ/公開CLI双方へ追加しました。種類キーは新規索引で生成されるため、旧索引は新規スキャンで再生成します。

`v1.1.1`では追加回帰試験のSQLite接続が閉じられず、Windowsで一時ディレクトリの削除に失敗しました。公開を停止し、`contextlib.closing`で読み取り接続を確実に解放する修正を`v1.1.2`へ反映しました。判定・件数は維持しています。

## Windows IMEの初回失敗と同一タグでの診断

`v1.1.2`のネイティブCI初回は、IME取消の2回目のEscapeで検索欄からフォーカスが外れ、その後の確定文字列が空になりました。入力部品・IME試験コードは成功済みのalpha.2から変更されていません。同じタグを別Windows環境で実行した[診断](verification/v1.1.2-windows-diagnostic.json)では、通常権限のUIA・キー入力・IME変換/取消・2ファイルのバイト一致がすべて成功しました。[実画面と結果](verification/v1.1.2-windows-ime.json)も保持しています。

公開用CIは失敗したWindowsジョブ全体を、コード・判定を変更せず再実行し、[2回目の実行で4環境すべて成功](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42438017936)しました。正式CIの全条件を満たしてから公開しています。公開実物の4環境検証も初回で成功し、結論と実行回数を最終記録に残しています。
