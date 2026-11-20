# Rescue v1.51.0

Parallels HDSとOpenVZ ploopのスナップショットを論理RAWへ変換し、その世代に残るファイルを復元できます。GUIでは「イメージをRAWに変換」を開き、DiskDescriptor.xmlと、その中に記載されたベース・差分ファイルをすべて指定します。

```sh
rescue import-image --image-type parallels --source DiskDescriptor.xml \
  --segment base.hds --segment snapshot.hds --output recovered.raw --json
```

HDSの旧形式と新形式、フラットRAWベース、ベースを含め8世代までの単一ストレージに対応します。対象世代の論理ディスクのSHA-256が手元にある場合は `--raw-sha256` に渡してください。単体の疎なHDSには親の情報がなく、チェックサムが必要です。フラットなメンバー、壊れたHDS、差分ファイルを単独で選んだ場合は、記述子とメンバーを揃えてください。[手順と制限](IMAGING.md#parallelsopenvzの仮想ディスク)

| 検証 | 結果 |
| --- | --- |
| 11通りのHDS・記述子から16 MiBの論理RAWを変換 | 11件すべて原本と全バイト・SHA-256が一致 |
| 各RAWからNTFSで削除した10形式を、削除前の配置で復元 | 同じ10原本が全11通りで一致 |
| 別にFAT32・ext4へ8形式ずつ書き込み、読み戻し・削除後にQEMUでHDS化 | 復元した計16件の全バイト・SHA-256が一致 |
| 親の欠落、異なる世代、読取不能、既存出力との衝突など | 51通りすべてで新しい出力を作らず拒否 |

OpenVZ ploop 1.15が書いた実スナップショットを使用し、独立にQEMU 6.2と10.0で親から展開した論理ディスクとも照合しました。FAT32・ext4での削除後の取り出しにはSleuth Kitを使用しました。[実イメージ・ログ・照合結果](../tests/parallels/README.md)

分岐、分割、暗号化、読み取れない親・データがある場合は、元の仮想化環境と必要な鍵から対象世代をRAWへ書き出してください。上書きやTRIM後に値が失われた領域は復元できません。v1.38.0では自動判別がHDSやXMLそのものをRAWとしてコピーした事例があるため、旧版の利用時には出力を元の論理ディスクの長さ・SHA-256と比較してください。[公開版での再現記録](../tests/parallels/public-v1.38-linux.json)

Linuxの実GUIでも3世代のHDSを順不同で指定して変換し、選択されたRAWからNTFSの10原本を照合しました。誤ったSHA-256では出力が作成されないことを確認しました。[GUIの照合・画面](parallels-gui.json) ｜ [GUI全体の回帰試験](gui-v1.51-results.json)
