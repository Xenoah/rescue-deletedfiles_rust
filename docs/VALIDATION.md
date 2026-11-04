# Alpha の検証記録

## v0.1のローカル環境

Linux x86_64、Rust 1.99.1。これはWindows実行時の検証とは区別する。

| 検証 | 結果 |
| --- | --- |
| ワークスペース全体のビルド | 成功 |
| Rustテスト | 9件成功。短い入力、runlist境界、負の差分、疎領域、fixup、不正属性等 |
| CLI/MCP統合 | 23項目に加えてLinuxのSIGINT終了を検証。6種類の正解ファイル、変更拒否、範囲制限、ページ取得、重複要求、取消等 |
| ntfs-3gで生成したメタデータ | 非residentの25,600バイトが正解と一致。元イメージのハッシュも不変 |
| GUI（Linux/Xvfb） | 日本語表示、入力、スキャン、候補選択、復元を操作。出力の正解一致を確認 |
| Windows MSVC向け | 全crate・test targetの型検査成功。リンク・実行は含まない |
| 配布通知 | Windowsのnormal/build依存170パッケージのライセンス本文を収集 |

ntfs-3gテストは、独立ツールで作った正規のNTFSメタデータについて、削除フラグだけを
テスト用イメージ内で変更したもの。Windowsによる実際の削除操作や媒体故障を検証したものではない。
テスト用イメージは毎回新しく作った通常ファイルに限定する。

## 再実行

    cargo test --workspace --locked
    cargo build --workspace --locked
    python scripts/e2e.py --binary target/debug/rescue
    python scripts/e2e_streams.py --binary target/debug/rescue
    python scripts/e2e_fat.py --binary target/debug/rescue
    python scripts/independent_ntfs.py --binary target/debug/rescue
    python scripts/independent_fat.py --binary target/debug/rescue
    cargo check --workspace --all-targets --target x86_64-pc-windows-msvc --locked

independent_ntfs.pyにはmkntfs、ntfscp、ntfsinfoが必要。
independent_fat.pyにはmkfs.fat、mcopy、mdel、mkfs.exfatが必要。
Windows上の統合テストでは実行ファイル名をrescue.exeにする。

## 別途確認する項目

v0.1.0-alpha.2のWindowsでのビルド、debug/release実行ファイルの統合テストは
[GitHub Actions](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41660787133)で成功。
公開ZIPをダウンロードし、公開SHA256SUMS.txtとの一致とGUI/CLI・MIT・依存通知の同梱を確認した。
alpha.1の公開CIはWindowsのライセンス通知生成で停止した。alpha.2では通知生成のUTF-8を明示し、
上流ライセンスを改行変換しないGit属性を設定した。期待SHA-256は変更せず、照合を維持する。
Windows GUIの手動操作・IME・アクセシビリティ、実媒体、性能目標、有料製品比較は未検証。
未対応機能と既知の制約は[ALPHA.md](ALPHA.md)を参照する。

## v0.2.0-alpha.1 追加検証

- LZNT1の重なり参照、短い入力、展開上限、破損入力群、4KiBクラスターの圧縮単位境界。
- scripts/e2e_streams.py: Linuxで13ストリームの正解一致、圧縮/非圧縮/疎領域の混在、
  断片化、resident/non-resident/圧縮ADS、日本語・パス記号の名前、MCPからの復元。
- セッションv1互換、改変されたストリーム名の拒否、暗号化ADSと通常ストリームの区別、
  壊れた圧縮データの未完了扱い。
- WindowsではRtlCompressBufferで生成する3種類の入力を追加し、16ストリームを照合。
  [Windows/Linux CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41664246239)は成功し、
  公開済みZIPのSHA-256一致も確認した。
- ntfs-3g由来の通常DATA（25,600バイト）と名前付きDATA（2,500バイト）の復元。

これは合成イメージとAPI由来の圧縮データの検証であり、Windowsの実ボリュームでの削除・復元、
破損媒体や有料ソフトとの比較を検証したものではない。

## v0.2.0-alpha.2 追加検証

- FAT32/exFATのCLI/MCP統合15項目、13ファイルの正解バイト一致。
  残存チェーン、連続配置、切断・循環、FATミラー／active FAT、割当競合、名前の推定、
  断片化したディレクトリー境界、日本語、254文字の名前、削除ディレクトリーを含む。
- 不正なブート・LFN・エントリー集合・名前ハッシュ・upcase・ビットマップの検出、
  巨大data lengthの拒否、セッションのオフセット改変拒否、スキャン／復元再探索の取消。
- mkfs.fatとmcopyで作りmdelで実際に削除した3ファイル（日本語名を含む）の正解一致。
- mkfs.exfatが生成したブート・FAT・ビットマップ・圧縮upcase表を使い、削除エントリーを
  明示的に合成したファイル10,040バイトの正解一致。exFATの削除操作自体はエミュレーション。
- 既存NTFSのRust 13テスト、CLI/MCP 24項目、圧縮/ADS 13ストリームもローカルで成功。
- FATのレコード／候補上限と事前取消のテストを追加し、Rust全体は14件成功。
- Linux/XvfbのGUIでexFATをスキャンし、名前クリック・一括選択・復元を操作。
  8ファイルの正解一致を確認。長い名前の表示を省略し、全文はツールチップで保持。
- [Windows/Linux CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41730155697)も成功し、alpha.2を公開済み。

## v0.2.0-alpha.3 追加検証

- 使用中ファイルとディレクトリーの交差参照をFAT32/exFATで検出し、ファイル内の偽エントリーを候補にしないこと。
- exFATのビットマップが空きと示しても、使用中ファイルから参照される範囲の復元を拒否すること。
- 使用中範囲の索引を独立したクラスター集合と照合。連続配置の大きな使用中ファイルは範囲として保持する。
- FAT32/exFAT統合は17項目、Rustは15件。

2026-11-03、[公開タグのWindows/Linux CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41731219100)は成功。
Windows debug/releaseのFAT32/exFAT・NTFS・圧縮/ADS・MCP統合と、Linuxの独立ツール検証を通過。
最終版のLinux/Xvfb GUIでもexFATの8ファイルを正解と照合した。

公開したrescue-0.2.0-alpha.3-windows-x64.zipをダウンロードし、SHA256SUMS.txtとの一致、
ZIP内のCRC、GUI/CLIのx64 PE形式、MIT・依存通知・文書を含む9ファイルを確認した。
ZIPサイズは3,439,643バイト。SHA-256:

    da23044a75971a92585c2a61d00e4284a159c082d41bc2fccf80c2fbe2678479

Windows GUIの手動操作、実媒体、有料製品との比較は引き続き未検証。

## v0.2.0-alpha.4 ローカル検証（2026-11-04）

- Rust 16テスト、Linux clippy/build、Windows MSVC全ターゲットのclippy（型検査）成功。
- `scripts/e2e_carve.py`：23項目、23ファイルのバイト一致。Python struct/zlibで生成したPNGの
  全合法色形式・ビット深度、Adam7、5種類のフィルター、分割IDAT、1MiB読取窓の境界を含む。
- CRC・Adler-32不一致、短い画素データ、不正フィルター、IDATの順序、巨大サイズ、APNG、
  未知のcriticalチャンク、重複IHDRの拒否。セッションのオフセット・ハッシュ改変を拒否し、
  偽のサイズ・名前は保存範囲と出力名に採用しない。空入力、同一PNGの異なる位置も検証。
- サンプル照合範囲外のPNGをCRCが合うように変更し、ソースの更新日時を戻した場合も、
  復元時のPNG全体ハッシュ再照合が拒否することを確認。
- 既存CLI/MCP 24項目、圧縮/ADS 13ストリーム、FAT32/exFAT 17項目・13ファイル、
  ntfs-3gおよびmkfs.fat/mtools/mkfs.exfatによる独立検証も成功。
- Linux/Xvfbの最終GUIでPNG探索・一括選択・保存を操作し、3ファイルの正解バイト一致を確認。
  4件の署名候補のうち、CRC不一致の1件は選択・保存対象から除外。
  実スクリーンショットを`docs/images/intelligence.png`に保存。
- `docs/intelligence.html`をChromiumで表示し、両方式の切替・aria-pressed状態・390px幅の
  横はみ出しなし・JavaScriptエラーなしを確認。1280px幅の実表示を`docs/images/technical-ui.png`に保存。
- 依存ライブラリの通知は引き続き170パッケージ。PNG/CRCのバージョンは既存のCargo.lockから変更なし。

2026-11-04、[タグのWindows/Linux CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41767420831)は5分52秒で成功。
Windows debug/releaseの4統合テスト群と配布処理を通過し、v0.2.0-alpha.4を公開した。

公開ZIP・単体HTML・2枚の画像をダウンロードし、全4ファイルのSHA256SUMS.txtとの一致を確認。
ZIPのCRC、12ファイルの構成、GUI/CLIのx64 PE形式、MIT・依存通知・文書・画像の同梱を検証。
単体HTMLもChromiumで両方式の切替・画像表示・外部通信ゼロ・JavaScriptエラーなしを確認した。
リリース本文に2枚の画像と技術説明が掲載されていることも確認。
ZIPは3,871,853バイト。SHA-256:

    36eaff3d9192488dbaa47a6c619ca1a1f66c9e5eeae73818165554fe471ae370

画面の入力は `python3 scripts/make_carve_fixture.py /tmp/<new-name>.raw` で独立生成できる。
GUIでPNG / Carvingを選び、対応候補3件を保存して正解バイト列と照合する。
Windows GUIの手動操作・実媒体・有料製品比較は未検証。

## v0.2.0-alpha.5：監査8件の修正（2026-11-04）

修正の対応表は[AUDIT-FIXES.md](AUDIT-FIXES.md)に収録した。

- Rust全体19テスト成功（既存core16件＋GUI3件）。Linuxのworkspace build/clippy成功。
- Windows MSVC全ターゲットのclippy成功。これはWindowsでの実行とは区別する。
- 新しい`e2e_audit.py`の16ケースがLinuxで成功。探索順反転・子孫、壊れた使用中チェーンの
  先頭と途中、exFATの4種の解析失敗、MCP出力先の差替え、深い階層7,000ファイルを含む。
- `e2e_carve.py`は30ケース・23ファイルの正解一致。PNGチャンクCRCが正しくても、zlib終端欠落、
  Adler-32欠落、余分な画素やzlibストリーム、PLTE不整合・範囲外インデックスを拒否する。
- NTFS CLI/MCP 24項目、LZNT1/ADS 13ストリーム、FAT32/exFAT 17項目・13ファイルも成功。
- ntfs-3g、mkfs.fat/mtools、mkfs.exfatの独立メタデータ検証も成功。
- Linux/Xvfbの最終GUIで、PNGスキャン・一括選択後に入力を変えると候補が消え、復元ボタンが
  無効になることを確認。再スキャンしてPNG 3ファイルを保存し、正解バイト列・SHA-256と照合した。
- FAT32のGUIで、根拠が「FAT値0」と表示されることを確認。
- 最終GUI・FAT根拠・技術説明UIの実画面を更新。技術説明UIはChromiumで両方式、
  aria状態、390px幅で横にはみ出さないこと、JavaScriptエラーなしを確認。
- Windows配布用のnormal/build依存172パッケージのライセンス本文を収集できた。

2026-11-04、[alpha.5タグのWindows/Linux CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41812660512)は4分46秒で成功。
Windows debug/releaseで監査回帰16ケース、PNG30ケース、既存NTFS・圧縮/ADS・FATの統合を通過した。

公開ZIP・単体HTML・3枚の画像をダウンロードし、5点すべてのSHA256SUMS.txtとの一致を確認。
ZIPのCRC、14ファイルの構成、GUI/CLIのx64 PE、MIT・依存通知172パッケージ、監査修正記録・実画面の同梱を検証した。
公開HTMLもChromiumで実画像、両方式の切替、8件の監査説明、外部通信ゼロ・JavaScriptエラーなしを確認。
リリース本文に修正表と3枚のスクリーンショットが掲載されていることを確認した。
ZIPは4,125,152バイト。SHA-256:

    f91525437802b142e9a31eadd79ed70ea170a41f841b78f65f853683ad459630

Windows GUIの手動操作、実媒体、有料製品比較は未検証。


## v0.2.0-alpha.6：詳細ビュー・検索設定・ZIP・各OS配布

ローカルLinuxでfmt/clippy、Rust 25項目（core 19、GUI 6）、既存のNTFS 24・圧縮/ADS 13・FAT 17・PNG 30・監査16ケースを通過。
新しい`e2e_features.py`の60項目は、35形式36ファイルのZIP出力、独立zipfileのCRC・バイト一致・ハッシュ、上書き防止、旧セッション互換、フォルダー境界・深度0・拡張子、carvingの厳密な範囲、NTFS SI優先・FAT/exFATのタイムゾーン、MCPを含む。
ZIPの取消と出力先差替えはRustで回帰検証する。

Linuxの全形式試験も972観測・768一致で通過した。この総計には意図した未対応・データ喪失条件が混在するため、一般的な復元率として扱わない。
管理情報が残る8条件は576/576一致。独立FSと実削除FAT32の条件・制限は[CLOUD-TESTS.md](CLOUD-TESTS.md)を継続する。

新しい実GUIでは設定からZIP復元まで操作し、36ファイルの正解SHA-256、CRC、36件の作成・更新日時、入力全体の無変更を確認した。
詳細・アイコン間の選択保持、グループの折り畳み、サイズの昇降順を操作した。
[機械可読結果](ui-flow-results.json)と[再現手順](UI-FLOW.md)に実行ファイル・出力・画面のSHA-256を記録。

ローカルで作成したLinux DEBを展開し、ポータブルtar.gzと実行ファイル・依存通知のバイト一致を確認。
両配布物のCLIを実行してバージョンと追加60項目を再検証した。DEBの実システムへのインストールと、この展開検証は区別する。

Windows/MSVC、Linux/glibc 2.35、macOS x64/arm64はそれぞれネイティブCIで検証する。
全ターゲットの成功と公開ファイルの検証結果は完了後に追記する。Windows/macOSの手動GUI、実媒体、有料製品比較は未検証。
