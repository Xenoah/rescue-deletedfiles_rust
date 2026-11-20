# Linux MDの実メンバーからの構成検出

Linux 6.12.107とmdadm 4.4が作成したRAID5・RAID6配列19組を使用しました。うち5組はこの試験用に作成し、残り14組のメンバーは [RAID6の配置試験](../raid6-linux) と共用しています。各組でext4をマウントし、10原本をコピーして比較し、削除・同期・アンマウントしました。Linux MDが読み出した論理RAW、停止後のメンバー、削除前のハッシュを含むゲストログを保存しています。5組のうち3組はQEMUのvirtioディスクが論理・物理セクター4096バイトを報告する構成です。[元データ・実測値](manifest.json)

| 管理情報 | RAID | 配置 | 論理セクター |
| --- | --- | --- | ---: |
| 1.0 | 5 | left-symmetric | 512 |
| 1.0 | 6 | left-symmetric-6 | 4096 |
| 1.1 | 5 | right-asymmetric | 4096 |
| 1.1 | 6 | right-asymmetric | 512 |
| 1.2 | 5 | right-symmetric | 4096 |
| 1.2 | 6 | 14通り | 512 |

各ディレクトリの `guest.txt.gz` はゲストの実行ログ、`a.raw.xz` などはメンバー全体、`logical.raw.xz` はLinuxが実際に読んだ論理ディスクです。追加5組の `guest.sh` は実行したゲスト側のコマンドです。廃棄可能な隔離VMの専用ディスクを使用しました。

試験ではRAWを逆順で渡し、RAID5は1台、RAID6は2台を省きます。mdadmが報告したUUID・世代・配置・スロット・個別の開始位置・長さと、保存したRescue構成を照合します。10形式を配置から復元し、Linux追加試験ではさらに論理ディスク全体を取得・比較し、削除前の位置を与えずに内容検索して復元します。全ファイルの全バイトとSHA-256を削除前原本に照合します。

同じ配列から実際に1台を故障扱いにして取り外し、残り4台で別の位置に書き込んだ世代も収録しました。旧メンバーと新世代を混ぜた入力を拒否し、現世代3台から書き込み後の論理ディスクが復元できることを確かめます。[変更前後の世代・SHA-256](changed-generation/manifest.json)

```sh
python scripts/e2e_md_superblocks.py --binary /path/to/rescue
python scripts/e2e_md_superblocks.py --binary /path/to/rescue --native --mdadm /usr/sbin/mdadm
```

実測用の外部ツールはmdadmとe2fsprogsです。停止済みの対応配列をRescueで読み取る際には、どちらも不要です。[ポータブル照合](../../docs/md-superblock-results.json) · [Linux照合](../../docs/md-superblock-native.json)
