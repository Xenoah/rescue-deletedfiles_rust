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
全ターゲットと公開物の完了結果は下記「alpha.6の公開と公開ファイル照合」に記録した。Windows/macOSの手動GUI、実媒体、有料製品比較は未検証。


4ターゲットの移植検証では、Windowsの出力先ハンドルがディレクトリー差替え自体を拒否する動作を確認した。回帰テストは差替え検出とOSによる差替え阻止を区別し、ハンドルを解放した後には同じrenameが成功することも確認する。
LinuxはGUI実行に必要なlibxkbcommon-x11等を実行依存へ追加し、ネイティブCIでDEB・ポータブルとGUI起動の検証を通過した。
WindowsクラウドではOpenGL 1.1しか提供されず旧描画方式が起動できなかったため、Windowsの描画をDirect3D 12のwgpuへ変更した。OSにあるFXCを使い、追加のDXC DLLは配布しない。復元エンジンと画面の操作は共通のまま維持する。

WindowsのDirect3D版は描画初期化・インストール／削除検証まで成功した。macOS Intelのコールド起動で固定3秒の判定が早すぎる事例があり、起動確認を最大30秒の初期化待ちと、その後3秒の生存確認に分けた。未初期化や途中終了は引き続き失敗とし、計測時間も検証JSONに記録する。


## alpha.6の公開と公開ファイル照合

2026-11-04、[リリースタグの全ネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41837157548)が成功し、[v0.2.0-alpha.6](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.6)を公開した。
Windows x64・Linux x64・macOS Intel/Apple Siliconのインストール版とポータブル版、計8点がダウンロード可能。

公開された38添付ファイルを再取得し、すべてSHA256SUMS.txt / PUBLISHED-SHA256SUMS.txtと一致した。
ポータブル内の8実行ファイルのPE/ELF/Mach-OとCPUを確認し、ネイティブCIが実行・検証したバイナリのSHA-256とも一致した。
WindowsはVCランタイムの静的リンクもPEのimportで確認。ZIPのCRC、Linux DEBの実ペイロードとポータブルの一致、macOS PKGのXARチェックサム・Payload/PackageInfo/Bom、.appの最低OSと版も確認した。
公開Linux CLIでも検索・日時・ZIP・MCPの追加60項目を再実行して成功した。

| OS / CPU | 管理情報が残る8条件の正解一致 | 実際に初期化した描画 |
| --- | --- | --- |
| windows x64 | 576 / 576 | Rescue renderer: Dx12; device: Cpu; adapter: Microsoft Basic Render Driver |
| linux x64 | 576 / 576 | Rescue renderer: OpenGL |
| macos x64 | 576 / 576 | Rescue renderer: OpenGL |
| macos arm64 | 576 / 576 | Rescue renderer: OpenGL |

この表は固定コーパス・限定条件の照合数。実媒体の一般的な復元率ではない。
macOSの署名でCLIハッシュが変わるため、公開署名後の実ファイルでも全8条件を再実行し576/576一致を確認した。署名前と署名後の検証記録は区別して保持した。
公開のインストーラー検証内容、バイナリの全ハッシュは[公開物照合JSON](public-release-v0.2.0-alpha.6.json)に収録した。
単体HTMLは3枚の画面を内包し、GUIの36ファイルZIP検証と画面ハッシュも照合した。macOSはアドホック署名のみで未公証。Windows/macOSの手動操作、実媒体、有料製品比較は未検証。

署名・梱包後の公開CLI検証は[追加ネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41840193814)でも56秒で成功した。リリースへ4環境のJSON、統合JSON/Markdown、PUBLISHED-SHA256SUMS.txtを追加し、元の32ファイルとSHA256SUMS.txtは変更していない。
公開された単体HTMLもChromiumで3画面・2方式・390px幅・外部通信ゼロ・JSエラーなしを確認。[ブラウザー記録](public-browser-v0.2.0-alpha.6.json)、[公開GUIでの36ファイル保存記録](public-gui-v0.2.0-alpha.6.json)も収録した。

## v0.2.0-alpha.7 ディスク対応

[タグCI 41879142077](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41879142077)が4環境で成功し、インストール版・ポータブル版を公開した。Windows 2022、Ubuntu 22.04、macOS 26 Intel／Apple Siliconを使用。macOSの配布最小OS設定は13。Rust共通28テストとWindows／macOS各1テスト、debug/releaseの6統合スイート、形式別・独立ツール・パッケージ・GUI描画初期化を検証した。

各環境でMBR・512/4096セクターGPTの648バイト照合に加え、新規読み取り専用loop/VHD/hdiutilの3パーティションで216照合が成功。作業領域・フォルダー復元・ZIP・MCPの同一媒体への出力を作成前に拒否した。元fixture全体のSHA-256も不変。管理情報のある8条件576件とMBR/GPT144件、独立ツール由来の条件は`format-results-*.json`、ディスク試験は`disk-results-*.json`へ記録した。実SSDのTRIMや一般的な復元率の測定ではない。

公開37添付ファイルを再取得してSHA-256を照合し、8配布物の構造・実行ファイルを検査した。記録は[公開配布物JSON](public-release-v0.2.0-alpha.7.json)。公開Linux GUIのSHA-256 `72f6c0f9877eef18d00938215b654cf4cf1349fbffa9bd0b53550ac23963a9bd`はパッケージの検証値と一致。実GUIからGPTイメージをスキャンして36件を選択・ZIP保存し、独立したPythonで全バイト・SHA-256・36日時が一致した。[GUI JSON](public-gui-v0.2.0-alpha.7.json)と実画面を保持する。これはイメージ補助モードのGUI操作で、デバイスAPIは別途CIの仮想媒体で検証した。

公開`intelligence.html`もブラウザーで4画面・3方式の切替、390px幅、JavaScriptエラー・外部通信なしを確認した。[ブラウザーJSON](public-browser-v0.2.0-alpha.7.json)。

## alpha.8 元の場所・同名処理（ローカル）

2026-11-04、Rust29テスト、clippy全workspace/all-targets、7つのCLI/MCP統合群を通過。追加61項目はPDFの元パス・36件の階層保存・既存保護・大小文字・リンク・JSON改変・512 MiB上限・ZIP CRC・MCP出力範囲を検証した。GUIでも元の場所へ36件、既存PDF保持、ZIP36件と日時36件、入力無変更を照合済み。技術説明HTMLは5画面・4方式・390px幅を外部通信/JSエラーなしで確認。

各OSのネイティブ実削除・同じボリュームへの復元と、公開した配布物の確認はそれぞれ完了後に記録する。GUIはLinux/Xvfb上の実操作で、Windows/macOSの手動GUI検証ではない。[保存仕様](SAME-LOCATION.md)、[追加結果](restore-results.json)、[GUI元の場所](gui-original-results.json)を参照。

Windowsの実NTFS仮想媒体で、空の非resident DATAがhighest VCN=-1を持つことを確認。alpha.8で空ストリームとして正しく解析する。データ・割当がある偽のsentinel、空のMFTは引き続き拒否し、回帰テストを追加した。

最終Linux debug GUI（SHA-256: 4e6f1c249fe3b9d022d08b338d1706c149729ac2d45b6e59beb239f528a78e0b）で元の場所へ36件・既存PDF保持・ZIP36件・36日時を再照合した。実画面と機械可読記録を更新済み。

## alpha.8 4環境ネイティブ検証

[最終CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41931322681)は8分23秒で成功。Windows x64、Linux x64、macOS Intel/Apple Siliconの4環境でRust29テスト・debug/releaseの7統合群・35形式・ディスク864件・実削除したPDFの元の場所へのCLI/MCP復元を通過した。WindowsはNTFS、Linux/macOSはexFAT。スキャン時に元ボリュームへ作業先を作らないことも確認。各インストール／ポータブルの8配布物の検証、GUI初期化、48添付ファイルの組立とSHA-256照合が成功。公開ダウンロードの検証とは区別する。

## alpha.8 公開版の実ファイル検証

[タグCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41932655121)は14分25秒で成功し、[alpha.8](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v0.2.0-alpha.8)を公開。8配布物と48一次添付、追加6検証レポートのチェックサムを実取得で照合した。Windows PE、macOS両CPUのMach-O、署名後のCLI/GUIハッシュ、PKGのXAR内容、DEBとポータブルの実行ファイル一致も確認。

[公開後CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41934558749)も4ネイティブ環境で成功（1分36秒）。実際の公開CLIをダウンロード・照合して実行し、各環境で35形式、ディスク864件、ZIP/日時/検索60項目、元の場所・衝突処理の追加試験、新規仮想ボリュームのPDF実削除・CLI/MCP復元2件を通過した。Macはアドホック署名後の実ファイルである。CI実行ID・SHAを含む追加JSONはリリースに添付済み。

公開Linux GUI（SHA-256: 2777e20c298b135e0a22925bbd38f68f32a582d459066851421eea0308e643ae）でも元の場所へ36件、既存PDF保持、36件スキップ、ZIP36件と表示日時36件、入力イメージの全体無変更を再確認。6画面を実撮影した。公開HTMLも5画面・4方式・390px幅でJSエラー／外部リクエストなし。[公開配布物](public-release-v0.2.0-alpha.8.json)・[元の場所](public-gui-original-v0.2.0-alpha.8.json)・[GUI ZIP](public-gui-v0.2.0-alpha.8.json)・[HTML](public-browser-v0.2.0-alpha.8.json)が機械可読記録。

実デバイス試験は新規の仮想媒体に限る。実SSDのTRIM、物理故障、未対応FS、商用製品との復元率比較、Windows/macOS GUIの手動操作は検証したと主張しない。

## 2026-11-05 JST / alpha.9 JPEG

Rust 31項目、fmt、workspace/all-targets clippy（警告をエラー扱い）、8統合群をローカルで通過。追加JPEG58項目は32枚＋埋込サムネイル33候補のCLI/MCP全バイト一致、破損33条件の拒否、PNG混在、範囲、拡張子同義語、ZIP、衝突、改変セッション、元ファイル変更の再検証を含む。独立エンコーダーの正解は tests/jpeg。35形式の900観測中730件が正解一致し、未対応・上書き済みケースは期待どおり未復元。これは一般的な復元率の分母ではない。

Linux実GUIでJPEG33候補のZIP、既存35形式36ファイルの元の場所／ZIP、既存PDF保持、36日時を照合。入力イメージ全体のSHA-256は不変。ネイティブ4環境と公開実行ファイルの検証はCIの機械可読記録に分けて記載する。

[ネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41938746415)の4環境すべてで追加JPEG58項目・既存8統合群・35形式・ディスク864件・仮想媒体の実削除PDF復元・配布物とGUI起動の検証が成功。4環境でJPEG33候補が一致し、破損33条件を拒否。公開後の実バイナリー試験は別記録にする。

alpha.9公開照合：タグCI41939770021成功。[実配布物CLIの4環境CI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41940947569)で各JPEG33候補／58項目、破損33条件、管理情報576件、ディスク864件、ZIP／日時等60項目、実削除PDF2件が成功。macOSはアドホック署名後のCLIハッシュと一致を確認。公開55一次ファイル＋6追加レポート（チェックサム一覧2点を含む総添付63）を全照合。公開Linux GUIもJPEG33件のZIP、CRC／SHA-256、入力無変更を確認。単体HTMLは6画面・5方式を390px幅を含め動作確認し、JavaScriptエラー・外部通信なし。[機械可読の記録](public-release-v0.2.0-alpha.9.json)。

## 2026-11-05 JST / alpha.10 文書・動画とGUI配置

LinuxローカルでRust33項目、fmt、workspace/all-targets clippy、9統合群が通過。新しい69ケースは正常23変種、破損/未対応30条件、CLI/MCPの30出力が正解一致。決定的な改変・切断1,024種も検査。35形式900観測中755件が正解一致（管理情報576件、連続carving11/36、断片carving2/36を含む）。

実GUIで元の場所36件と既存PDF保持、ZIP36件と36日時、JPEG33件、文書/動画7件を照合。Ctrl+Fで結果検索へ移動し、絞込み後も選択7IDが保たれることを実操作。最小720×520の横はみ出しを発見・修正し、3幅×3倍率×3画面状態の主要ボタン位置を回帰検査。Windows/macOSの手動アクセシビリティ試験とは区別する。ネイティブVM/公開実物の結果は完了後に追記する。

最終変更後もRust33項目・文書69ケース・Linux/Windowsターゲットのall-targets clippyを通過。GUIの4復元シナリオは最終debugバイナリーで再実行し、各JSONのハッシュを更新済み。上書き済みデータの試験は、Quickが現在のバイトを保存する一方で削除前の正解とは0件一致であることを確認するもので、内容の完全性を検出したとの意味ではない。

[alpha.10のネイティブCI](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/41971886923)は4環境すべて成功（8分57秒）。debug/releaseの9統合群、文書69ケース、35形式、各864件の仮想ディスク、実削除PDF、GUI起動、8配布物の検証と全体アセンブルを通過。公開実物の検証は別途追記する。
