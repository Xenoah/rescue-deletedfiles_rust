# PhotoRecでファイルを探す

CLIの `photorec` は、管理情報の失われたディスクやRAWイメージをローカルのPhotoRecで全域検索します。保存先の下に毎回専用フォルダーを作り、候補ファイルとSHA-256一覧を保存します。TestDiskに含まれる `photorec`（Windows配布では `photorec_win.exe`）を別途インストールし、PATHに追加してください。

```sh
rescue photorec --source disk.raw --destination recovered --json
rescue photorec --source disk.raw --destination recovered --all-families --block-size 512 --json
```

通常はPhotoRecの既定の形式で検索します。`--all-families` はそのビルドが提供するすべての形式を有効にします。探索位置はブロック境界です。`--block-size` には512～1,048,576バイトの2の累乗を指定でき、省略するとPhotoRecが選びます。パーティションの空き領域だけに限定する操作ではありません。仮想ディスクは `import-image` で論理RAWに展開してから検索してください。欠損セクターを記録したRescueの読取マップ付きRAW・RAIDマニフェストは内蔵スキャナーで扱います。

出力JSONの `directory` が今回の出力先、`manifest` が行単位のJSON一覧です。各行の `path` は出力先からの相対パスで、`bytes` と `sha256` は実際に保存した内容を表します。Unixのファイル名をUTF-8で表現できないときは `path_unix_bytes`、Windowsでは必要に応じて `path_utf16` に元の表記を残します。PhotoRecのログとXMLは一覧のファイル数に含めません。中止すると実行中のリーダーを止め、今回の一時フォルダーを破棄します。

同じ36原本をブロック境界に置いたLinuxの試験では、PhotoRec 7.2の既定設定で29候補、希少形式を含む全設定で28候補が作られました。APNG・ICO・MP3・XML・XZは両方の設定で原本と全バイトが一致しました。既定設定は23種、全設定は21種の拡張子で一致を確認しました。形式同士が同じ部分を候補にする場合があり、有効にする形式を増やしても一致数が増えるとは限りません。元の配置や削除状態は照合できないため、候補の中身は原本のハッシュやアプリケーションで確認してください。
