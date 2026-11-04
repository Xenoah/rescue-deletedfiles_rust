# Rescue

Rust製のWindows向けファイル復元ソフトウェア。GUI・CLI・MCPで共通の復元エンジンを使います。
復元できるファイル数と内容の完全性を最優先に、NTFS、FAT32/exFAT、破損媒体、RAIDへ段階的に対応します。

現在の実装は **v0.2.0-alpha.5：NTFS・FAT32・exFATのRAWイメージ復元とPNG carving** です。
[この版の対応範囲と制限](docs/ALPHA.md)を確認してください。
実ディスク・RAIDとPNG以外のcarvingはまだ未対応です。有料製品を上回る性能は未検証です。

- [製品仕様](docs/SPEC.md)
- [フェーズと進捗](docs/GOAL.md)
- [リリース](https://github.com/Xenoah/rescue-deletedfiles_rust/releases)

監査で確認した8件を修正しました。探索順による誤復元、使用中領域の見落とし、MCPの保存先差替え、
PNGの不完全な圧縮データ、大容量セッション、割当警告、GUIの入力同期・根拠表示を改善しています。
[修正内容と回帰テスト](docs/AUDIT-FIXES.md)を参照してください。

## Recovery Intelligence

![PNG carvingと検証根拠を表示する実際のRescue GUI](docs/images/intelligence.png)

Linux / Xvfb上の実GUIで合成イメージを操作した画面です。PNGの3ファイルを保存し、正解バイト列との一致を確認。
Windows GUIの手動試験は別途必要です。

右側のパネルに**配置・形式の根拠 → 割当と元の名前 → 保存と照合**を表示します。
PNGの「検証済み」はチャンクCRC、zlib終端・Adler-32・展開長、パレット、画素展開の成功、「completed」は保存完了を示します。
どちらも削除前の内容との一致率ではありません。

[技術説明のインテリジェンスUIをダウンロード](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/download/v0.2.0-alpha.5/intelligence.html)してブラウザーで開くと、
Quick / Carvingの探索手順と制限を切り替えて確認できます。配布ZIPにも収録し、リリースには単体HTMLも添付します。
GitHubのREADME・リリース本文にはスクリーンショットと説明を掲載します（本文内ではHTMLの操作はできません）。

![QuickとPNG carvingの技術説明UI](docs/images/technical-ui.png)

| 方式 | 検出根拠 | 検証範囲 | 分からないこと |
| --- | --- | --- | --- |
| Quick | MFT / FAT / NoFatChain | 管理情報・配置・一部の割当競合 | 一般の内容の完全性、NTFS割当競合 |
| PNG carving | PNG署名と連続したチャンク | 全チャンクCRC・画素展開・復元時のPNG全体ハッシュ | 元の名前・階層・割当・削除状態 |

## Windowsで使う

配布ZIPを展開し、rescue-gui.exeを起動します。ボリュームイメージ、作業フォルダー、復元先を選択し、
スキャン結果からファイルを選んで復元します。元イメージは読み取り専用です。入力・探索方式を変更すると古い候補を無効化します。
通常ファイル、LZNT1圧縮、ADSの別ファイル保存に対応しています。
FAT32/exFATは残存チェーン、既知の先頭クラスター、exFATの連続割当を使います。
失われた断片位置は推測しません。候補の「要確認」「復元不可」に理由を表示します。
PNGは探索方式から「PNG / Carving」を選びます。ファイルシステムに依存せず通常のイメージ全体を探索します。
使用中ファイルや埋込画像も検出します。静止PNGの連続配置に限定し、APNG・断片の再構成は未対応です。
復元先にはNTFS等のハードリンク対応媒体を使います。
Windows x64と[Microsoft Visual C++ v14 x64ランタイム](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)が必要です。

CLIの例（PowerShell）:

    .\rescue.exe scan --source 'D:\Images\volume.raw' --work-dir 'E:\RescueWork' --json
    .\rescue.exe results --session 'E:\RescueWork\<id>.rescue-session.json' --json
    .\rescue.exe recover --session 'E:\RescueWork\<id>.rescue-session.json' --id mft-20-7 --destination 'E:\Recovered' --json

PNG探索の例：

    .\rescue.exe scan --mode carve --source 'D:\Images\disk.raw' --work-dir 'E:\RescueWork' --json

scanが返すsessionと、resultsが返す候補IDを使います。全対応候補を選ぶ場合は--idの代わりに--allを指定します。
セッションは128MiB以内に収めます。上限到達時は候補の省略とtruncatedを明示し、保存済みの途中結果を読み込めます。
出力ファイル名にはUUIDが付き、既存ファイルを上書きしません。結果とSHA-256はJSONレポートに保存します。
処理が完了しても、上書きやTRIMで失われた元データを復元できたとは限りません。

## MCP

MCPクライアントから次のローカルstdioコマンドを起動します:

    rescue.exe mcp --stdio --allow-source 'D:\Images\volume.raw' --work-dir 'E:\RescueWork' --allow-output-root 'E:\Recovered'

list_sources、start_scan、get_job、list_candidates、recover_files、cancel_job、get_reportを提供します。
start_scanとrecover_filesはrequest_idを要求し、job_idを即座に返します。get_jobで終了を確認します。
start_scanのmodeはquick（既定）またはcarve。progress.unitでrecords / bytesを識別します。
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
    python scripts/e2e_carve.py --binary target/debug/rescue.exe
    python scripts/e2e_audit.py --binary target/debug/rescue.exe
    cargo run -p rescue-gui --locked

Linuxでは統合テストの実行ファイルをtarget/debug/rescueにします。
テスト用イメージは人工生成し、実媒体を変更しません。製品の主対象はWindowsです。

タグによるGitHub ActionsはLinux/Windowsの検証後にWindows用ZIPとSHA-256一覧をプレリリースします。
ソースのみのpushを、実行ファイルの公開完了とは扱いません。

## ライセンス

[MIT](LICENSE)。依存ライブラリのライセンスは配布ZIPのTHIRD_PARTY_NOTICES.txtに収録します。
