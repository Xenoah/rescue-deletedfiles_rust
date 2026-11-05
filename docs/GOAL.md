# 実装目標と進捗

ユーザーの指示: 仕様に沿ってフェーズごとに自律的に実装・検証・push・リリースする。
対象と最優先指標は[SPEC.md](SPEC.md)を参照する。
このファイルは工程の記録であり、バックグラウンドの自動実行予約ではない。

| フェーズ | 完了条件 | 状態 |
| --- | --- | --- |
| 仕様 | 対象全カテゴリ、GUI/CLI/MCP、比較・受入基準 | mainに公開済み |
| v0.1 alpha | NTFS RAWの基本復元、GUI/CLI/MCP、正解付きテスト、Windows成果物 | v0.1.0-alpha.2公開済み。Windows/Linux CI成功、公開ZIPのSHA-256一致 |
| v0.2 alpha | FAT32/exFAT、圧縮/ADS、主要形式のcarving | 圧縮/ADS・FAT32/exFATの基本復元をv0.2.0-alpha.3で公開済み。Windows/Linux CI・公開ZIPのSHA-256一致を確認。PNG carving・根拠を示すGUI・技術説明UIをv0.2.0-alpha.4で公開済み。Windows/Linux CI・全配布物のSHA-256一致を確認。alpha.9でJPEGを4環境検証・公開済み。alpha.10でPDF／ZIP／Office／MP4／MOVと応答的なGUI配置を実装し、公開前検証中 |
| v0.3 alpha | 実デバイス、読取マップ、イメージ取得、論理破損 | alpha.7でデバイスの読み取り専用アクセスとMBR/EBR/GPTを先行実装。読取マップ・イメージ取得は未完了 |
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
alpha.6の[3 OS版リリース](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.6)と
[成功したタグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41837157548)を確認済み。
次はPDF/ZIP/MP4等の主要形式へcarvingを拡張する。FATのチェーン喪失時の推測や壊れたブート領域の探索は未完了。
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
各OSのCI・インストーラー検証、公開成果物の取得確認を経てalpha.6を公開済み。実装済みと公開済みは区別して記録する。
新しい画面とSHA-256付きのGUI検証は[UI-FLOW.md](UI-FLOW.md)。従来の監査修正と形式別試験を継続する。

alpha.6は[全4ネイティブ環境のタグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41837157548)が成功し公開済み。8つのインストール／ポータブル配布物と、添付38点のSHA-256を照合した。詳細・アイコン・設定フローとREADME／リリースの実画面・技術説明UIも更新済み。JPEG等のcarving、実デバイス、RAID、商用比較は引き続き後続工程。

## alpha.7 ディスク入力

ユーザー指定に合わせ、ディスクをGUIの既定入力にした。全パーティションの候補を集約し、元ディスクへの作業・復元を拒否する。CLIのsources / inspect / check-destinationと、明示的allowlistを維持したMCPも同じディスク読取を使う。各OSで新規仮想媒体を読み取り専用に接続する試験と、画像・技術UI・配布物の検証をリリース条件にする。

alpha.7は[タグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41879142077)で4ネイティブ環境が通過して[公開済み](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.7)。8配布物・37添付ファイルの実ダウンロード照合と、公開Linux GUIの36件ZIP復元も確認した。物理故障・読取マップ・イメージ取得・再開・RAID再構成・商用比較は後続工程。

## alpha.8 元の場所と同じディスク

ユーザー指定に従い、別ディスク限定を解除。元の場所・フォルダー・ZIP、連番／スキップ、同名フォルダーの統合を共通化した。選択データを全て先に読み出し、合計512 MiB上限では出力作成前に停止する。ローカル追加61項目と実GUIの元の場所への36件・ZIP36件・36日時を照合済み。4 OSネイティブの実削除・同じボリュームへの復元をリリース必須条件にする。詳細は[SAME-LOCATION.md](SAME-LOCATION.md)。公開済みのalpha.7とは区別して記録する。

alpha.8の[4ネイティブ環境CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41931322681)が成功。新規仮想媒体の実削除・元の場所へのCLI/MCP復元、8配布物と全添付の組立まで通過した。タグ発行後、公開した実ファイルを再取得して照合する。

alpha.8は[タグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41932655121)で4環境の検証を通過して[公開済み](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.8)。公開8配布物・48一次添付・追加6レポートを実取得で照合し、[署名／梱包後CLIの4環境検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41934558749)と公開Linux GUIの元の場所36件・ZIP36件・既存保持／スキップも通過した。元の場所・同名処理の今回の実装と公開は完了。PNG以外のcarving・イメージ取得・RAID・商用比較は後続工程。

## alpha.9 JPEG探索

通常・プログレッシブJPEGを、独立した圧縮データ検査と画素展開で検証する。32枚＋埋込サムネイル33候補、追加58項目（破損33条件）、実GUIのJPEG ZIP33件を照合済み。PNGとの混在、旧セッション読込、元の場所／ZIP36件も維持。4環境のネイティブCIと公開配布物そのものの試験を経て公開する。詳細は[JPEG-CARVING.md](JPEG-CARVING.md)。以降のPDF／ZIP／MP4、読取マップ、RAID、商用比較は後続工程。

alpha.9の[4ネイティブ環境CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41938746415)でWindows・Linux・macOS Intel／Apple Siliconの試験、インストール版とポータブル版、GUI起動が通過。JPEG追加58項目と新規仮想媒体の実削除・同じ場所へのPDF復元を含む。タグによる再検証と公開実ファイルの照合へ進む。

alpha.9は[タグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41939770021)を通過して[公開済み](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.9)。8配布物・55一次添付・6追加レポートを実取得で照合。[公開後の4環境試験](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41940947569)は署名／梱包後CLIのJPEG33候補と破損33条件、既存機能、実削除PDFを確認。公開Linux GUIのJPEG33候補のZIPと単体HTML6画面・5方式も通過。今回のJPEGフェーズは完了。次はPDF／ZIP／MP4等のcarving、以降は読取マップ・イメージ取得・RAID・商用比較を進める。

2026-11-05: v1.0全必須機能を完了する追加指示を受け、[受入台帳](V1_ACCEPTANCE.md)と[UIの公式資料・採用規則](UI_GUIDELINES.md)を作成。各OSの実VMと使い捨てディスクで共通・固有シナリオを検証する。


alpha.11で読取マップ付き取得と再開を追加。Rust38項目、10統合群、取得追加15条件、実GUIの512MiB取得と取得後36ファイルを確認。各OSの新規読取専用仮想ディスク取得、容量不足からの再開、権限拒否をCIへ追加。v1.0の残作業は受入台帳を維持する。

alpha.11: 4ネイティブCI 41979409874が成功。イメージ取得/マップ/再開/容量不足/権限拒否の区切りを公開し、配布実物を照合する。次のNTFS属性リスト・$Bitmap・複数名を別の作業ブランチで実装中。

alpha.11公開実物まで完了。タグCI41980373634/公開CI41981560219成功、83資産照合、公開GUIの512 MiB取得・再開・36件復元、HTML11画面/7方式を確認。次工程のNTFS拡張は22条件・独立ntfs-3gの21ストリームをローカル検証し、ネイティブ/GUI/公開物の検証を準備中。v1.0の残る必須項目は継続する。

alpha.12準備: 属性リスト・分割MFT・$Bitmap・別名。ローカル40 Rust/11統合群/NTFS22条件、独立生成21ストリームと実GUIを検証。Windows自身の20 ADS実削除シナリオを追加し、ネイティブ実行を待つ。
