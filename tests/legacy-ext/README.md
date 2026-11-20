# ext2・ext3の空き領域とLinuxで削除した原本

Linux 6.12.107の仮想マシンで、試験用のディスクだけをext2またはext3で初期化しました。同じ9本を `deleted` と `saved` に書き込み、実際に読み戻し、前者だけをunlinkしました。アンマウント後にゲストが計測したディスク全体のSHA-256を取り出したRAWと照合しました。原本にはBMP、GIF、JPEG、PDF、PNG、SQLite、WAV、WebP、ZIPを使用しています。

[manifest.json](manifest.json) に各原本の長さとSHA-256、削除済み・使用中の2つの物理バイト位置、ディスク全体のSHA-256を記録しました。チェック対象のXZファイルはこのRAWの可逆圧縮です。Linux上で同じ条件を作り直すには、試験専用の仮想マシンで下記を実行してください。生成スクリプトは一時ディレクトリーに作成した通常ファイルだけをループバックでマウントします。作り直すとUUID・日時・ブロック位置が変わる場合があります。

```sh
sudo python tests/legacy-ext/generate_linux.py /tmp/legacy-ext-fixtures
python scripts/e2e_legacy_ext.py --native --binary target/release/rescue
```

ext2とext3は1、2、4 KiBブロックの6構成を用意しました。さらに機能フラグのないext2 revision 0と、3段間接ブロックを実際に使うext3を収録しています。後者には約68 MiBのスパースファイル、同じinodeへのハードリンク、短い・長いシンボリックリンクがあります。ファイルの割り当てはe2fsck、Sleuth Kitのfls・icat・blkstat、debugfsで独立に照合します。構造情報やブロックの所有関係を変えた異常系と、保存済みセッションの再検証は新しいコピーの上で実施します。
