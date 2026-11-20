# Rescue v1.52.0

Linux MDのRAID6で、固定・回転P/Q、Qを末尾に固定した配置、DDF互換の計14配置から削除ファイルを復元できます。メンバーのRAWコピーを用意し、GUIの「RAIDを構成」でmdadmと同じ配置名を選んでください。CLIでは構成JSONを保存し、そのファイルをスキャン元に指定できます。

```sh
rescue raid --config array.json --output recovered.rescue-raid.json --json
rescue scan --source recovered.rescue-raid.json --mode carve --work-dir scan --json
```

ディスクの順序、データ開始位置、使用長、チャンクサイズが必要です。欠けたメンバーは元のスロットに残し、JSONでは `path` に `null` を指定してください。配列メンバーを復元先として開くことはありません。[14配置の名前と設定](RAID.md#linux-mdのraid6配置)

| 独立に照合した項目 | 結果 |
| --- | --- |
| Linux MD・mdadmで作成した5台構成の14配列。ext4で各10形式を書き込み・読み戻し・削除 | 計140件の削除をゲスト内で確認 |
| 各配列で2台を欠損としてRescueが読み出した全12 MiB | 14件すべてLinuxカーネルの原本と全バイト・SHA-256が一致 |
| 取得後のRAWを内容検索し、BMP・FLAC・GIF・JPEG・PDF・PNG・SQLite・WAV・WebP・ZIPを復元 | 140件すべて原本と全バイト・SHA-256が一致 |
| 各配列の欠損なし、1台欠損5通り、2台欠損10通りで、2回転分の非ゼロデータを確認 | 14×16通りで一致 |
| Linuxカーネルが2台欠損で読み取り専用に組み立てた論理ディスク | 14×10通りで健全時の全体SHA-256と一致 |

3台が同じ範囲で読めない場合はその範囲が欠損します。一部のメンバーだけにある取得マップの局所的な欠損も試験しました。2台欠損でさらに読み出せたバイトが壊れる場合や、整合して見える別の配置を選んだ場合は、原本のSHA-256が必要です。構成が変化している途中のリシェイプと独自コントローラーの配列情報は対象外です。DDF互換の試験はLinux MDのデータ配置を検証しています。[実データ・生成ログ・比較結果](../tests/raid6-linux) ｜ [ポータブル試験](raid6-layout-results.json) ｜ [Linux試験](raid6-layout-native.json)

Linuxの実GUIでは、DDF N-continue配置で2台が欠けた構成をスキャンし、10形式をZIPに保存しました。全ファイルが原本と一致し、出力レポートにも読み取りに使った配列と欠損スロットが記録されました。[GUI実測・画面](gui-v1.52-results.json)
