# Rescue v1.9.0

拡張子もファイルシステムも特定できない断片化ファイルを、既知の配置表と原本のSHA-256から復元できるようになりました。GUIとCLIで利用できます。圧縮したRAWディスクを直接取り込めるようになり、Apple DMGではLZFSE・LZMA圧縮も展開できます。

## ファイル形式が分からないときの復元

設定画面で元のディスクまたはイメージを選び、「ディスクのツール」→「配置表からファイルを復元」を開きます。JSON配置表と新しい保存先を指定すると、記録された順番で各領域を読み取り、保存前にファイル全体のSHA-256を照合します。

```sh
rescue recover-extents --source disk.raw --layout layout.json --output recovered.bin --json
```

配置表には、原本のSHA-256と、各断片の先頭位置・長さをバイト単位で指定します。位置を知るには、たとえば削除前のブロック配置記録が必要です。明示したスパース領域、同じ領域の再使用、RAIDの論理アドレスにも対応します。読み取り不能な領域、ハッシュ不一致、既存の保存先では新しい出力を確定しません。[配置表の例](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.9.0/docs/IMAGING.md)

## ディスクイメージ

| 入力 | 対応内容 |
| --- | --- |
| gzip・bzip2・XZ・Zstandard・LZ4圧縮のRAW | GUIで変換するか、CLIの `import-image` で展開。同じ方式の連結ストリームとXZの規格に沿ったパディングに対応。論理ディスクのバイト数とSHA-256を記録 |
| Apple ULFO・ULMO | LZFSEとXZ形式のLZMAを使う単独UDIFイメージ。ブロックごとの長さ・ディスク内の位置・記録されたCRC-32を照合 |

```sh
rescue import-image --source backup.img.zst --image-type compressed-raw --output backup.raw --json
rescue import-image --source backup.dmg --output mac.raw --json
```

圧縮RAWは1イメージあたり16 TiB、同じ圧縮方式の65,536ストリームまでです。ディスク取得後に上書きされた領域や、SSDのTRIMで消去された内容を再現する機能はありません。

## 実証結果

- FAT32で実際に7ファイルを削除した68 MiBのディスクを、それぞれ独立した5種類の圧縮コマンドで保存しました。GUIで全5種類を開き、展開後のディスク全体のSHA-256と、削除されたPNG・WebP・ICO・テキスト・XML・ZIP・bzip2を照合しました。計35ファイルすべてが原本と一致しました。
- macOSの `hdiutil` で同じディスクのULFO・ULMOを作成して確認しました。両方とも71,303,168バイトの論理ディスク全体が原本に一致し、7ファイルずつ計14件を復元できました。AppleのC実装またはPythonのliblzmaを使う独立した展開でも照合しました。[Apple生成イメージの記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.9.0/tests/udif/apple-reference/manifest.json)
- 配置表による復元では、独自形式・空ファイル・スパースファイルを含む11原本を照合しました。別に、ext4で削除前に `debugfs bmap` により記録した配置から実際に削除した8ファイルを復元しました。読み取りマップに欠損として記録した領域は、その内容が本来ゼロでも復元を拒否しました。[再現テスト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.9.0/scripts/e2e_known_extents.py)
- GUIの配置表操作でもTXT・CSV・AAC・HTML・TAR・PNG・独自形式の断片化ファイル7件が一致しました。異なるSHA-256を指定した試験ではエラーを表示し、保存先にファイルを作成しませんでした。[GUIの画面と照合結果](gui-v1.9-results.json)
