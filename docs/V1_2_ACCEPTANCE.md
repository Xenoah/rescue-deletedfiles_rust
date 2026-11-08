# v1.2 次段階の受入台帳

2026-11-08 UTC。v1.1.2後にユーザーから進行指示を受けた、破損USB/SDへの対応強化を扱います。APFS等の別フェーズは含めません。

| 段階 | 範囲 | 状態 |
| --- | --- | --- |
| alpha.1 | 大ブロックの順方向取得、逆方向の細分化、セクター再試行、読取待機の上限、同一ソースを確認した再接続 | 4環境ネイティブCI・4環境公開実物の検証合格、公開完了 |
| alpha.2 | FAT 8.3名の明示的CP932解釈、元バイトと解釈根拠の保持 | 4環境ネイティブCI合格・公開済み、公開実物の検証中 |
| alpha.3 | exFATの破損メタデータに対する限定的な継続と、根拠不足の明示 | 79 Rustテスト・34統合群・実GUI・Linux配布物合格、ネイティブ/公開検証待ち |
| 安定版 | 利用者の既知ハッシュ一覧との照合、統合検証と最終UI記録 | 82 Rustテスト・35統合群・実GUI・Linux配布物合格、4環境検証待ち |

## 取得強化の完了条件

- 既読範囲を再取得せず、順方向→逆方向・小ブロック→セクター単位の取得量を記録する。
- 読取の待機時間を制限し、遅い結果を誤って保存しない。OS内で停止したI/Oの物理的な即時取消は保証しない。読み取り処理が終了しなければ新しい処理を無制限に増やさず停止する。
- 再接続はソースID・容量・セクターサイズ・通常ファイルの更新時刻を照合し、別媒体への入れ替えを拒否する。
- RAWの同期後だけ成功範囲を読取マップに追記する。中断・強制終了・容量不足・欠損伝播・出力先保護を維持する。
- 旧読取マップの再開を維持する。新しい取得方式はマップに記録し、再開時の解釈を変えない。
- 正解データ付きの一時的/恒久的な読取失敗、遅延、切断・再接続を試験する。実ハードウェア故障の測定とは区別する。

各段階でローカルの回帰、4環境のネイティブCI、公開した実物のテストを実行します。画面は実際のGUIで操作して撮影し、対象バージョンとファイルのハッシュを記録します。配布添付は8パッケージと共通SHA-256の9点を維持します。

## alpha.1 ローカル結果

Rust 75テスト、32統合群、Clippy、fmt、配布添付の2契約テストが合格。
Linux DEB/portableの内容検証と、同梱CLI/MCPの60項目が合格。
読取プロセス停止・取消・再接続・差し替え・旧マップ互換の13項目が合格。
実GUIは36ファイル・152,325バイトを復元し、HTML/CSVの内容がJSONと一致。
取得GUIでは1秒の期限超過で4 MiBを失敗、60 MiBを未読として保持し、再開後の64 MiBすべてが元SHA-256と一致した。

[ローカル集計](verification/v1.2.0-alpha.1-local.json) · [取得試験](acquisition-v1.2.json) · [GUIと画面ハッシュ](gui-v1.2-results.json) · [画面](UI-v1.2.md)

これらは合成障害/所有ファイルによる検証で、物理故障媒体の復旧率ではない。

## alpha.2 ローカル結果

CP932の34条件で168出力がバイト一致。FAT12/16/32、JSONセッション/索引/MCP、MBR/GPT、プレビュー、ZIP、HTML/CSV、強制終了後の再開を確認した。
新しいCP932セッション/索引はスキーマ10を使い、v1.2.0-alpha.1の実CLIが拒否することを確認した。旧ASCIIセッションは新しい版でも復元できる。

[CP932の条件と結果](cp932-v1.2.json) · [旧版との実物確認](verification/v1.2.0-alpha.2-compatibility.json) · [文字コードと名前の不確実性](CP932.md)

実GUIでCP932の8ファイル・224バイトと、従来の36ファイル・152,325バイトが一致。取得の期限超過後に64 MiBを再開し、全バイトが元イメージと一致した。実画面18枚とハッシュを記録した。

[alpha.2ローカル集計](verification/v1.2.0-alpha.2-local.json) · [実画面](UI-v1.2.md)

alpha.1は[全4環境のCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42471633276)に合格し、[9添付で公開](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0-alpha.1)済み。[公開実物の再検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42474088556)は全4環境で合格。

[alpha.1公開後の全記録](verification/v1.2.0-alpha.1-published.json) · [リリース確認](verification/v1.2.0-alpha.1-release.json)

## alpha.3 ローカル結果

大文字変換表のチェックサム・展開内容の破損を明示的に許可する31条件で、72出力が元データと一致。表の欠落・重複・サイズ/チェーン不正、割当情報の欠損、エントリチェックサム/UTF-16破損、ライブ所有競合は従来どおり拒否または未対応とした。正常な表の名前ハッシュ検証は維持した。

JSON/索引・MBR/GPT・CLI/MCP・プレビュー・ZIP/HTML/CSV・プロセス強制終了後の再開を確認。スキーマ11をalpha.2の実物CLIが拒否することも確認した。

[exFAT破損表の条件と制限](EXFAT-DAMAGE.md) · [旧版との実物確認](verification/v1.2.0-alpha.3-compatibility.json)

実GUIで36ファイルの従来フロー、CP932の8ファイル、破損exFATの8ファイルを照合。取得のタイムアウトと64 MiB全量の再開も確認し、実画面20枚を保存した。

[alpha.3ローカル集計](verification/v1.2.0-alpha.3-local.json) · [exFAT試験の全条件](exfat-damage-v1.2.json) · [実画面](UI-v1.2.md)

## v1.2.0 安定版のローカル結果

既知SHA-256照合の18条件で32出力のバイト一致を確認。CLI・MCP・GUIで一致/一覧外/照合不能を区別し、名前の不確実性、元の警告、欠損範囲を維持する。照合一覧のラベルをファイルパスとして開かない。保存後に変えたファイルを再検査したかのように表示しない。

82 Rustテスト、35統合群、Clippy/fmt、配布添付・インストーラ版番号の3テストが合格。Linux DEBとportableの実物、および同梱CLI/MCPの60項目が合格した。先行版から安定版へのWindows/macOS内部版番号の逆戻りも修正した。

実GUIでは36件の従来フロー、CP932の8件、破損exFATの8件、64 MiB取得の中断/再開、照合結果の保存を確認。26枚の実画面とSHA-256を保存した。

[安定版ローカル全記録](verification/v1.2.0-local.json) · [既知ハッシュ試験](known-hashes-v1.2.json) · [実GUIと画面](UI-v1.2.md)

alpha.2: [ネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42474565163)合格、[公開](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.2.0-alpha.2)済み。[公開実物検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/42477620895)を実行中。
