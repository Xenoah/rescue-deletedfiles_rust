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
