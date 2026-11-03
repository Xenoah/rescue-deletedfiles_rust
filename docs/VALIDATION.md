# Alpha の検証記録

## ローカル環境

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
    python scripts/independent_ntfs.py --binary target/debug/rescue
    cargo check --workspace --all-targets --target x86_64-pc-windows-msvc --locked

independent_ntfs.pyにはmkntfs、ntfscp、ntfsinfoが必要。
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
- WindowsではRtlCompressBufferで生成する3種類の入力を追加し、16ストリームを照合するCIを用意。
  native Windowsの結果は公開後に追記する。
- ntfs-3g由来の通常DATA（25,600バイト）と名前付きDATA（2,500バイト）の復元。

これは合成イメージとAPI由来の圧縮データの検証であり、Windowsの実ボリュームでの削除・復元、
破損媒体や有料ソフトとの比較を検証したものではない。
