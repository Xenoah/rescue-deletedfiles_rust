# Linux MDのミラーとRAID10

Linux 6.12.107とmdadm 4.4で、RAID1を2台・3台・4台、RAID10 near=2を4台で作成しました。管理情報1.0・1.1・1.2と、論理・物理セクター512バイトまたは4096バイトのvirtioディスクを使用しています。さらに同じ4台構成でfar=2とoffset=2も作成しました。各配列でext4にBMP・FLAC・GIF・JPEG・PDF・PNG・SQLite・WAV・WebP・ZIPをコピーして読み戻し、10件を削除して同期・アンマウントしました。停止前にLinuxが読んだ12 MiBの論理RAW、停止後のメンバー、実行コマンド、SHA-256を含むゲストログを収録しています。[実測値と原本](manifest.json)

対応する5配列で、生き残ったメンバーの空集合以外をすべて試しました。RAID1では1台以上、near=2では隣り合う各ペアから1台以上という条件を満たす43通りを受理し、30か所の相異なる非ゼロデータを照合しました。ペアの両側が欠けた12通り、および異なる配置で作成した本物のfar=2・offset=2を拒否しました。各対応配列を必要最少のメンバーで取得し、Linuxの論理RAWと全バイト・SHA-256を比較しました。復元した10形式50件は配置指定と通常の内容検索の双方で原本と一致しました。

独立検証では元のメンバーを保ったまま使い捨てのオーバーレイを介し、Linuxが `mdadm --assemble --readonly --run` で最少のメンバーを読み取り専用で組み立てました。RAID1の単独9通り、RAID10の各ペアから1台ずつの8通りで、カーネルの全体SHA-256が健全時の原本と一致しました。各ディレクトリの `degraded.txt.gz` が実行ログです。

ミラー片側だけに読取不能区間がある例、両側から読めるバイトが違う例、一方しか残っておらずその内容が壊れている例も試験しました。最後の例は原本のSHA-256があれば誤った復元を拒否できます。MD管理情報の機能フラグ付き・同期中の配列は範囲外です。far/offset配置は別コピーをLinux MDで読み取り専用に組み立て、論理RAWを取得してください。Rescueでの対応配列の読み取りに外部ツールは不要です。

```sh
python scripts/e2e_md_mirrors.py --binary /path/to/rescue
python scripts/e2e_md_mirrors.py --native --binary /path/to/rescue --mdadm /usr/sbin/mdadm
```

[全メンバー選択パターン](../../docs/md-mirror-results.json) · [Linuxで復元した各ファイルのSHA-256](../../docs/md-mirror-native.json)
