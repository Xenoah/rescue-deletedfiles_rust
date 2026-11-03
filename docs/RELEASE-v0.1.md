NTFSボリュームのRAWイメージから削除ファイルを取り出す最初のalphaです。

- Rust製の共通エンジンをGUI、CLI、ローカルstdio MCPから操作できます。
- resident/non-resident DATA、断片化したrunlist、疎領域、日本語名を扱います。
- ソースは読取専用。UUID付き出力、SHA-256、JSONレポートを保存します。
- 公開ワークフローはLinux/WindowsのテストとCLI/MCP統合検証の成功を要求します。

実デバイス、FAT32/exFAT、RAID、carving、圧縮/暗号化、属性リストはこの版では未対応です。
出力先にはNTFS等のハードリンク対応媒体が必要です。GUIのプレビューと途中継続も未対応です。
Windows GUIの手動評価と有料製品との比較結果はまだありません。
保存完了は元のデータとの完全一致の保証ではありません。

ZIPにGUI/CLI、MITライセンス、依存ライブラリの通知、対応範囲を同梱します。
未署名の開発版です。SHA256SUMS.txtでダウンロードしたZIPを確認できます。
