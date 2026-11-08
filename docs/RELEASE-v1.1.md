# Rescue v1.1.0-alpha.2

USB・SDなどのFAT復元を拡張した、v1.1へ向けた先行版です。

## 変更

- FAT12/16の固定ルート・長い日本語名・サブディレクトリー・残存する断片化チェーンを解析。
- チェーンが消えたFAT12/16/32は、詳細設定で明示的に有効化した場合だけ連続配置を推定。割当競合、読み取れないFAT、形式検証で不正な推定を拒否します。
- 推定したファイルには、選択画面から保存結果まで「推定・内容未確認」の表示を保持。
- アイコンに形式ごとの色と拡張子を追加。前版の設定カード、レスポンシブ配置、現在のステップ表示も継続。
- 添付はインストーラー/ポータブル8点とSHA256SUMS.txtの計9点だけです。GitHubの自動ソースコード表示は別枠です。

## 実測と実画面

同じFAT32比較イメージで、標準14/36件、推定有効時36/36件が元のSHA-256と一致しました。GUIでも36件をZIPへ復元して照合しました。FAT12/16のCLI・索引・MCPで60件、独立した実削除で8件を照合しています。断片化した未知形式が誤った推定になる試験も残し、内容一致数へ算入しません。

![推定を明示的に有効化](https://raw.githubusercontent.com/Xenoah/rescue-deletedfiles_rust/v1.1.0-alpha.2/docs/images/v1.1-alpha.2/settings-ja.png)

![推定を区別した保存結果](https://raw.githubusercontent.com/Xenoah/rescue-deletedfiles_rust/v1.1.0-alpha.2/docs/images/v1.1-alpha.2/recovered-ja.png)

[対応範囲・制限と検証](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.1.0-alpha.2/docs/FAT-v1.1.md) · [実操作記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.1.0-alpha.2/docs/gui-v1.1-results.json)。各OSの検証資料はActionsのnative-*、公開後の再検証はpublished-verificationへ保存します。公開後の結果リンクを追記します。

## 制限

推定は初期状態で無効です。構造が正しくても元の内容との一致は保証されず、保存完了は内容の正しさを意味しません。1ファイル64 MiBまで。HTML/CSVレポートは安定版へ向けた次の段階です。APFS/ext4直接解析は未対応です。

Windowsは発行元署名なし、macOSはアドホック署名でApple公証なしです。
