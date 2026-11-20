# Rescue v1.40.0

VMDKのスナップショットで、選んだ世代から削除ファイルを復元できます。対象の子VMDKを指定し、直近の親からベースまで順に選びます。QEMUが記録する親CIDとファイル名、VirtualBoxが記録するイメージUUIDと更新UUIDを照合し、子が書き換えた領域、消去した領域、親から読み継ぐ領域を組み合わせます。

```sh
rescue import-image --image-type vmdk --source child2.vmdk \
  --segment child1.vmdk --segment base.vmdk --output guest.raw --json
rescue scan --source guest.raw --work-dir scan --json
```

GUIでは「イメージをRAWに変換」でVMDKを選び、親を1行に1ファイルずつ指定します。完全な論理ディスクの長さやSHA-256が分かる場合は、GUIの照合欄またはCLIの `--raw-bytes`、`--raw-sha256` で保存前に確認できます。変換にQEMUやVirtualBoxのインストールは不要です。

## 実ディスクイメージでの結果

| 作成元とゲスト | 世代数 | 最後の世代から復元したファイル |
| --- | ---: | --- |
| QEMU 10・Windows NTFS | 3 | AVI、DOCX、FLAC、PPTX、SQLite、TAR、WAVの7件 |
| Oracle VirtualBox 7.2.22・同じWindows NTFS | 3 | 親をUUIDだけで識別するチェーンから同じ7件 |
| QEMU 10・圧縮ベースとWindows NTFS | 4 | 64 KiBの圧縮グレインと32 KiBの子を組み合わせて同じ7件 |
| QEMU 6.2・FAT32 | 3 | BMP、GIF、PDF、PNG、WAV、WebP、ZIPの7件 |
| QEMU 10・ext4 | 8 | 同じ7形式を1世代ごとに1件ずつ削除 |

21世代すべてで、Rescueが保存したディスク全体のバイト列とSHA-256が原本と一致しました。QEMU形式の18世代はQEMU、Oracle形式の3世代はVirtualBoxでも読み直しました。各構成の最後の世代から通常スキャンと索引付きスキャンの両方で復元した計35件は、全バイトとSHA-256が原本と一致しました。複数の構成で同じ原本を使用しています。[テストイメージと作成方法](../tests/vmdk-chain/README.md) · [世代別・ファイル別の照合結果](vmdk-chain-native-results.json)

ext4ではThe Sleuth Kitでも全世代を開き、削除件数が0件から7件まで1件ずつ増えました。各世代で取り出した延べ28件について、名前、inode番号、全バイト、SHA-256を確認しました。FAT32では、子でゼロにした16グレインに親の内容が戻らないことをディスク全体の比較で確認しました。

以前の一部の公開版ではVirtualBox製の子VMDKだけを指定すると、親に残っているデータが欠けたRAWが保存されました。この版では、UUIDだけで親を識別するVMDKも親子一式を直接読み取れます。実際の96 MiBの3世代すべてがWindowsで読んだ論理ディスクと一致しました。GUIでも最後の世代を読み、NTFSから削除した7ファイルをZIPに保存し、展開した106,654バイトを原本と照合しました。同名の別の親を選んだ場合は変換を拒否しました。[以前の公開版での実測値](../tests/vmdk-vbox-parent/README.md) · [GUIの記録と画面](gui-v1.40-results.json)

## 対応範囲と制限

- 子はディスクリプターを内蔵した非圧縮のmonolithicSparse、ベースはmonolithicSparseまたはzlib圧縮のstreamOptimizedに対応します。ベースを含めて最大8ファイル、グレインの対応表は合計128 MiBまでです。全世代の論理容量が一致する必要があります。
- 親の欠落・順序違い、CID・UUID・更新UUIDの不一致、矛盾する親参照、継承する圧縮データの破損を拒否します。記録上の識別子を保ったまま圧縮していないデータが変わった場合には、既知の論理ディスクSHA-256との照合が必要です。[拒否条件の実測結果](vmdk-chain-results.json)
- 親の名前を記録する方式では、ディレクトリ部分のない名前を照合します。パス付きの親参照、分割スナップショット、VMFS・SEsparse、暗号化ディスクは、対応するVMware製品などで全世代を開いてRAWに変換してください。暗号化を解除するには鍵が必要です。失われた親、上書きやTRIMで消えた内容を補うには、元のコピーまたは別のスナップショットが必要です。

Rustテスト96件、Clippy、統合試験99組が通りました。ファイル形式の回帰試験では35形式・36原本を1,116条件で比較し、完全一致を期待する1,016条件すべてで原本と一致しました。
