# Rescue v1.19.0

VMwareのVMDKをGUIとCLIで読み取り、復元元のRAWに変換できます。単独のmonolithicSparse・streamOptimizedは1ファイルを指定します。monolithicFlat・twoGbMaxExtentFlat・twoGbMaxExtentSparseでは記述ファイルと構成ファイルをすべて指定します。

```sh
rescue import-image --source guest.vmdk --segment guest-s001.vmdk --segment guest-s002.vmdk --image-type vmdk --output guest.raw --json
rescue scan --source guest.raw --work-dir scan --json
```

## 実測

各試験で、Windows上のNTFSから実際に削除したHTML・JSON・RTF・XML・PNG・ZIP・gzipの7原本と、復元ファイルの全バイトおよびSHA-256を照合しました。

| イメージまたは操作 | 結果 |
| --- | --- |
| 単独VMDKの6パターン | 疎・明示ゼロ、先頭と末尾の圧縮グレイン表、フッターのみの参照、部分グレインを検証。QEMUで論理ディスクを独立に比較し、6パターンそれぞれで7/7件が原本と一致 |
| フラット・分割VMDKの6パターン | 明示したベース名を記述ファイルに照合。構成ファイルの逆順指定、セクターオフセット、同じファイルの別範囲、ZERO宣言、3個の疎エクステントに対応。6パターンともゲスト全体と7/7件が一致 |
| 2 GiBを超える実際のQEMU分割 | フラット型・スパース型とも、QEMUが自動分割した2ファイルから2,213,809,152バイトを再構成。境界前の3件と境界以降の4件を復元し、ディスク・ファイルとも全バイトが一致 |
| GUI | 単独疎・圧縮・フラット・3分割の4パターンを変換し、各RAWがスキャン元に選ばれたことを確認。分割ゲストの削除済み7件はGUIでもZIPに復元し、645バイトすべてを原本と照合 |
| 不完全・破損した入力 | 単独疎19例、圧縮13例、記述ファイルと構成ファイル25例で変換を拒否。2 GiB超の試験でも2番目の構成ファイルを除くと未完成のRAWを残さず停止 |

[単独VMDKの参照イメージ](../tests/vmdk/README.md) · [分割VMDKの参照イメージ](../tests/vmdk-split/README.md) · [Windowsでの削除原本](../tests/windows-vhdx/README.md) · [GUI操作とSHA-256の記録](gui-v1.19-results.json)

## 対応条件

対応するVMDKの読み取りには外部プログラムは不要です。分割VMDKでは構成ファイルの指定順を問わず、記述ファイル内の配置に従います。ファイル名を変更した構成ファイル、親が必要な差分、VMFS、暗号化、古いCOWDには対応する仮想化環境や変換ツールが必要です。元データが上書きされた場合や、必要な親・構成ファイル・復号鍵が失われた場合には元の内容を保証できません。フラット領域や通常の疎グレインの内容を確認するには、独立に取得した原本のハッシュが必要です。[取り込み手順](IMAGING.md)

独立コーパス35形式・36ファイルの1,116件の観測中、原本と一致したのは1,016件でした。管理情報喪失後の連続配置は33/36件、断片化配置は12/36件、対象データ上書き後は0/36件です。
