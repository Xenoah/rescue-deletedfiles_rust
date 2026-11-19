# Rescue v1.11.0

削除前に記録した断片ごとのハッシュを使い、ディスク上の位置が分からないファイルを復元できます。APFSでは、削除前のスナップショットが残っていれば、事前にハッシュを記録していないファイルも取り出せます。

## 位置が分からない断片から復元する

GUIでは「ディスクのツール」から「断片ハッシュでファイルを探す」を開きます。原本または同一のコピーを開けるうちに「原本から記録を作成」でJSONを別の媒体に保存します。復元時には設定画面で対象ディスクを選び、「断片を探して復元」でJSONと出力先を指定します。CLIで512バイト単位の記録を作る例です。

```sh
rescue fingerprint-file --source original.bin --chunk-size 512 --output original.chunks.json --json
rescue recover-fingerprinted --source disk.raw --manifest original.chunks.json --output restored.bin --json
```

この記録には原本サイズ、各断片の照合値とSHA-256、原本全体のSHA-256が入ります。検索は原則1バイトずつ開始位置をずらし、順番が入れ替わった断片も探します。各断片を照合して元の順番に並べ、全体のSHA-256が一致したときに新規ファイルを確定します。完全に同じ断片が複数あった場合は、同じ読み取り可能なデータを再利用できます。

- 記録単位は512バイトから1 MiBの2のべき乗を指定でき、既定は4 KiBです。ファイル当たり最大65,536断片です。細かい単位ほど小さい断片を探せますが、記録が大きくなります。
- 配置が分かっている場合、CLIの `--start` と `--end` で検索範囲をバイト単位で絞れます。全断片の開始位置がセクター境界だと分かっていれば `--alignment 512` を使えます。
- 断片内でさらに分断された場合、上書き・TRIM・読取不能で一致する断片を得られない場合、全体SHA-256だけが手元にある場合は、この方法による再構成はできません。読取マップで欠損と記録された領域は照合から除外します。内容の一致はファイル名や元の物理位置の証明にはなりません。

## APFSのスナップショットから復元する

APFSを読めるThe Sleuth Kitの `pstat`、`fsstat`、`fls`、`icat` が必要です。最初にボリュームとスナップショットの一覧を表示し、実際に返った `volume_block` と `xid` を選びます。次は同梱の検証用ボリュームでの例です。

```sh
rescue apfs --source volume.raw --json
rescue apfs --source volume.raw --volume-block 32770 --snapshot-xid 3 --json
rescue apfs --source volume.raw --volume-block 32770 --snapshot-xid 3 --all --destination restored --json
```

ディスク全体のイメージは `--sector-offset` でAPFSコンテナの先頭を指定できます。一部のファイルを取り出すには `--all` の代わりに一覧にある `--id` を指定します。過去のスナップショットにあり、同じパスが現在のボリュームにない通常ファイルを抽出します。名前変更や移動も候補に含まれます。削除前のスナップショットと読み出せる実データが必要です。検証したのは暗号化されていないAPFSです。FileVaultの解錠、消去済みスナップショットの再生、クラウド上にだけあるファイルの取得は対象外です。

## 実証結果

- FAT12にTXT・CSV・HTML・WAV・ZIP・独自バイナリ・反復データの7原本を書き、各ファイルが複数の離れたクラスタに分かれたことを `mshowfat` で確認しました。7件を `mdel` で実際に削除してから、配置表なしで復元し、すべてのバイト列とSHA-256を原本と照合しました。[再現テスト](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.11.0/scripts/e2e_fingerprinted.py)
- これとは独立に、任意のバイト位置に断片を並べ、空・短いファイル・ゼロ埋めを含む10原本を復元しました。入力のSHA-256が誤っている場合や読取マップの欠損箇所だけが一致する場合に、誤った出力を確定しないことも確認しました。
- 160 MiBのAPFSボリュームでスナップショットを作成後、PNG・XML・TXT・ZIP・MP3・RTF・WebPを実際に削除しました。Rescueで単一RAWとオフセット付き分割RAWから各7件を取り出し、全バイト列とSHA-256を照合しました。現在も残るファイルと撮影後に追加したファイルは候補から外れました。macOS標準のAPFSドライバーでも現在と過去を別々に読み取り専用でマウントし、同じ7原本を照合しました。[試験イメージと原本ハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/v1.11.0/tests/apfs-reference) · [Linuxでの作成と検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44580072513) · [macOSでの独立検証](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44582256202)
- Linuxの実GUIでも7原本から各512バイト単位の記録を作り、実際に断片化・削除したFAT12から7件すべてを復元しました。入力ディスクのSHA-256は操作前後で一致し、原本全体のSHA-256を意図的に変更した試験では出力を拒否しました。[GUIの試験記録・画面](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.11.0/docs/gui-v1.11-results.json)

35形式・36原本の既存試験では、管理情報が残るNTFS・FAT32・exFATの8条件で、CLIとMCPを通じて576/576件のバイト列が一致しました。削除管理情報を消し、事前の断片ハッシュも持たないNTFSでは、連続配置で31/36件、断片化配置で11/36件でした。元データを上書きした条件では一致は0件でした。
