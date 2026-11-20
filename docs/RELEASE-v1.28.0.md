# Rescue v1.28.0

初代QCOW v1の仮想ディスクをGUI・CLIから直接RAWに変換できます。スパース領域、通常クラスタ、バイト単位の長さを持つDEFLATE圧縮クラスタに対応します。外部ツールのインストールは不要です。

```sh
rescue import-image --source archive.qcow --image-type qcow1 --output archive.raw --json
rescue scan --source archive.raw --work-dir scan --json
```

GUIでは「イメージをRAWに変換」で「QCOW / QCOW2」を選びます。変換したRAWが次のスキャン元になります。CLIで `--image-type` を省略しても、ヘッダーからv1とQCOW2を識別します。

## QEMUと原本による実証

| 元ディスク | 通常のスパースQCOW | 圧縮クラスタを含むQCOW |
| --- | --- | --- |
| FAT32 | 削除済み7件を原本と照合 | 同じ7件を照合 |
| ext4 | 削除済み7件を原本と照合 | 同じ7件を照合 |
| Windows NTFS | 元のファイル名で削除済み7件を照合 | 同じ7件を照合 |

FAT32・ext4にはBMP、GIF、PNG、PDF、WebP、WAV、ZIPを保存し、実際に読み戻してから削除しました。Windows NTFSではWindowsで削除したHTML、JSON、RTF、XML、PNG、ZIP、gzipの原本を使いました。QEMUで作成した6イメージのゲスト全体をQEMUの独立展開と比較し、復元ファイルも全バイト・SHA-256を照合しました。[全ディスクとファイルのハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.28.0/docs/qcow-v1-results.json)

512 B、4 KiB、32 KiB、64 KiBのクラスタと、64〜8,192項目のL2テーブルを組み合わせた5配置でも、RAW全体がQEMUの展開と一致し、各7ファイルを原本どおりに復元しました。全24種類の破損・未対応ケースはRAWを出力せず終了しました。[5配置の測定結果](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.28.0/docs/qcow-v1-geometry.json)

QEMU 6.2.0（Ubuntu 22.04）と10.0.13（Debian）の両方で、実削除の6イメージと5配置を独立に展開して照合しました。Linuxの実GUIではFAT32、ext4、Windows NTFSの3イメージを変換し、選択されたRAWから各7件を復元して原本と照合しました。暗号化指定のイメージでは理由を表示し、RAWの出力を拒否しました。[GUIの実測記録](gui-v1.28-results.json)

ローカルではRustテスト96件、厳格なClippy、統合試験86組を通過しました。35形式・36原本の比較試験では、欠損や上書きを含む1,116条件のうち1,016条件が原本と完全一致し、各条件の期待値に一致しました。

## 対象と制限

親を参照しない単独イメージが対象です。親を必要とする差分イメージ、暗号化イメージ、同じデータを複数回参照する割り当て、テーブルとデータの重複、不完全な圧縮ストリームを拒否します。親や鍵がある場合は、対応するQEMU環境で展開したRAWをスキャンできます。上書き・TRIMで失われたバイトには別のコピーが必要です。初代QCOWの圧縮ストリームは原本のチェックサムを持たないため、展開できた内容の照合には独立した原本ハッシュを利用できます。[取り込み手順と上限](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.28.0/docs/IMAGING.md)
