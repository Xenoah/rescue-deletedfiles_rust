# alpha.8 元の場所への復元

Linux / Xvfbの実GUIで、合成NTFSボリュームから35形式・36ファイルを元の階層へ復元した。既存のsample.pdfを残し、復元側はsample (復元 1).pdfになった。全36件の正解バイト・SHA-256と入力イメージの無変更を照合。[元の場所の記録](gui-original-results.json)を参照。

同じ実行で詳細列・元フォルダーのグループ・アイコン表示・選択保持を操作し、その後ZIP保存でも36件と36日時を照合した。[GUIフロー記録](ui-flow-results.json)の6画面はalpha.8の実画面。元ボリュームのルートはテスト用に明示した。ネイティブの自動判定は各OSの仮想媒体試験で区別する。

技術説明UIには元の場所への復元モードを追加。Windows/macOSの手動GUI操作とは区別する。検証スクリプトは保存後の照合用で、GUI自動操作を行うものではない。

---

# alpha.7 ディスク入力とGUI検証

既定のディスク選択画面を実撮影。撮影したクラウド環境はデバイスが非公開のため、直接アクセスは各OSの読み取り専用仮想ディスク試験で別途検証する。GUIは補助イメージモードからGPT内の合成NTFSを探索し、35形式・36ファイル、全36日時を確認してグループ・選択・ZIP復元を実操作した。独立したPython ZIP展開で全バイトとSHA-256が一致し、元ディスクイメージの全体ハッシュも一致した。

実画面とGUI実行ファイルのハッシュは[ui-flow-results.json](ui-flow-results.json)、正解データは[ui-demo-manifest.json](ui-demo-manifest.json)。Linux debug GUIの記録であり、公開配布物のハッシュやWindows/macOSの手動操作とは区別する。ディスク選択・設定・詳細・アイコンの4画面と、Disk・Quick・PNGの3方式を切り替える技術説明UIは、390px幅を含めJavaScriptエラー・外部リクエストなしを確認する。

---

# alpha.6：GUIの操作と復元検証

2026-11-04、Linux / Xvfb、1260×820、Rust debug版の実GUIをXTestのマウス・キーボード入力で操作した。
画面はモックではない。Windows/macOSの手動GUI検証とは区別する。

確認した操作：

1. 起動時に設定画面が開く。イメージを入力し、ZIP保存と保存先を指定した。
2. 詳細設定を開き、Quick、拡張子、フォルダー、深度、作業領域の入力欄を確認した。
3. スキャン完了で候補選択へ遷移し、36候補を表示した。作成・更新日時も36件すべてフィクスチャ値と一致した。
4. 元のフォルダーでグループ化。サイズの昇順は33 B、55 B、61 B…の数値順、再クリックで降順を確認した。
5. 2ファイルを選び、詳細とアイコン表示を切り替えて選択が保持されることを確認した。
6. グループの折り畳みと展開、表示中の全対応候補の選択を操作した。
7. 復元を実行し、36ファイルのZIPを保存した。独立したPython zipfileでCRC、内包レポート、全ファイルのサイズ・SHA-256・正解バイトを照合した。
8. 元イメージの全体SHA-256は操作前後で一致した。

[機械可読結果](ui-flow-results.json)には、出力ZIP・GUI実行ファイル・入力・3枚の画面のSHA-256と36ファイルの照合値を記録した。
READMEの画面は保存前に2件を選択した状態で、全件復元はその後に実行した。

入力は35形式・36ファイルの実データを断片化して配置した合成NTFS。
6つのフォルダーと作成・更新日時は画面用の明示したフィクスチャ値で、実際に削除した媒体の日時ではない。
元のファイルデータは[tests/corpus](../tests/corpus)と同じ。画像生成や動画生成をGUI検出の代わりには使っていない。

再現：新しい出力先で次のコマンドを実行し、生成されたイメージをGUIの設定で指定する。

```sh
python scripts/make_ui_fixture.py /tmp/new-ui.raw --manifest /tmp/new-ui-truth.json
cargo run -p rescue-gui --locked
```

設定→スキャン→ファイル選択→ZIP復元を操作したあと、保存したファイルを検証できる。
`verify_gui_flow.py`は保存後の検証用で、GUIを自動操作するスクリプトではない。

```sh
python scripts/verify_gui_flow.py --truth /tmp/new-ui-truth.json --source /tmp/new-ui.raw --work /tmp/output/.rescue-work --archive /tmp/output/recovered.zip --gui target/debug/rescue-gui --output /tmp/ui-proof.json
```

検証スクリプトはdocs/images内の3画面もハッシュ化する。画面を再撮影する場合は同じ1260×820で保存する。
CLI/MCP側の絞り込み・日時・ZIP・既存保護・旧セッション互換は`e2e_features.py`の60項目、
ZIP取消・出力先差替えとGUIの入力・選択・数値／日時の並べ替えはRustテストで補っている。


## 公開ポータブルでの再検証

公開Linux x64ポータブルのGUI本体（ネイティブCIの検証済みSHA-256と一致）でも、同じ設定・スキャン・グループ・選択・ZIP復元を実行した。36ファイルの正解SHA-256、36件の日時、元イメージの全体無変更を再確認した。[公開GUIの照合記録](public-gui-v0.2.0-alpha.6.json)と[保存完了の実画面](images/public-saved-view-v6.png)に収録した。既存の開発版GUI記録とは実行ファイルのSHA-256を区別している。
