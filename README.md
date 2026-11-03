# Rescue

Rust製のWindows向けファイル復元ソフトウェア。GUI・CLI・MCPで共通の復元エンジンを使います。
復元できるファイル数と内容の完全性を最優先に、NTFS、FAT32/exFAT、破損媒体、RAIDへ段階的に対応します。

現在の実装は **v0.2.0-alpha.2：NTFS・FAT32・exFATボリュームのRAWイメージ復元** です。
[この版の対応範囲と制限](docs/ALPHA.md)を確認してください。
実ディスク・carving・RAIDはまだ未対応です。有料製品を上回る性能は未検証です。

- [製品仕様](docs/SPEC.md)
- [フェーズと進捗](docs/GOAL.md)
- [リリース](https://github.com/Xenoah/rescue-deletedfiles_rust/releases)

## Windowsで使う

配布ZIPを展開し、rescue-gui.exeを起動します。ボリュームイメージ、作業フォルダー、復元先を選択し、
スキャン結果からファイルを選んで復元します。元イメージは読み取り専用です。
通常ファイル、LZNT1圧縮、ADSの別ファイル保存に対応しています。
FAT32/exFATは残存チェーン、既知の先頭クラスター、exFATの連続割当を使います。
失われた断片位置は推測しません。候補の「要確認」「復元不可」に理由を表示します。
復元先にはNTFS等のハードリンク対応媒体を使います。
Windows x64と[Microsoft Visual C++ v14 x64ランタイム](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)が必要です。

CLIの例（PowerShell）:

    .\rescue.exe scan --source 'D:\Images\volume.raw' --work-dir 'E:\RescueWork' --json
    .\rescue.exe results --session 'E:\RescueWork\<id>.rescue-session.json' --json
    .\rescue.exe recover --session 'E:\RescueWork\<id>.rescue-session.json' --id mft-20-7 --destination 'E:\Recovered' --json

scanが返すsessionと、resultsが返す候補IDを使います。全対応候補を選ぶ場合は--idの代わりに--allを指定します。
出力ファイル名にはUUIDが付き、既存ファイルを上書きしません。結果とSHA-256はJSONレポートに保存します。
処理が完了しても、上書きやTRIMで失われた元データを復元できたとは限りません。

## MCP

MCPクライアントから次のローカルstdioコマンドを起動します:

    rescue.exe mcp --stdio --allow-source 'D:\Images\volume.raw' --work-dir 'E:\RescueWork' --allow-output-root 'E:\Recovered'

list_sources、start_scan、get_job、list_candidates、recover_files、cancel_job、get_reportを提供します。
start_scanとrecover_filesはrequest_idを要求し、job_idを即座に返します。get_jobで終了を確認します。
許可した入力と出力先だけを使い、復元ファイルの内容はMCPの結果へ送りません。

## 開発

Rust 1.99.1、Python 3.12以降。WindowsではMSVCのC++ Build Toolsが必要です。
依存関係はCargo.lockに固定しています。

    cargo build --workspace --locked
    cargo test --workspace --locked
    cargo clippy --workspace --all-targets --locked -- -D warnings
    python scripts/e2e.py --binary target/debug/rescue.exe
    python scripts/e2e_streams.py --binary target/debug/rescue.exe
    python scripts/e2e_fat.py --binary target/debug/rescue.exe
    cargo run -p rescue-gui --locked

Linuxでは統合テストの実行ファイルをtarget/debug/rescueにします。
テスト用イメージは人工生成し、実媒体を変更しません。製品の主対象はWindowsです。

タグによるGitHub ActionsはLinux/Windowsの検証後にWindows用ZIPとSHA-256一覧をプレリリースします。
ソースのみのpushを、実行ファイルの公開完了とは扱いません。

## ライセンス

[MIT](LICENSE)。依存ライブラリのライセンスは配布ZIPのTHIRD_PARTY_NOTICES.txtに収録します。
