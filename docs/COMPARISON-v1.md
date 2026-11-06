# v1.0 同一イメージでの商用製品比較

2026-11-06、Linux x64の同じ実行環境で、固定した3個の64 MiBイメージを
Rescue 1.0.0、UFS Explorer Standard Recovery 10.23、DMDE 4.4.4へ渡した。
画面の成功表示ではなく、保存したファイルのサイズ・全バイト・SHA-256を採点した。
**この範囲では商用製品を上回っていない。FATチェーン消失ではRescueが劣る。**

| 入力と削除状態 | Rescue 1.0.0 | UFS Explorer 10.23 | DMDE 4.4.4 |
| --- | ---: | ---: | ---: |
| NTFS、ntfs-3gで作成、MFTの使用中ビットを消した削除模擬 | 36/36 | 36/36 | 36/36 |
| FAT32、dosfstools/mtools、実際の`mdel`でチェーン消去 | 14/36 | 36/36 | 36/36 |
| exFAT、exfatprogsで作成、未割当領域へ削除エントリーを配置した模擬 | 36/36 | 36/36 | 36/36 |

各行は同じ35形式36ファイル、合計152,325バイト。最大ファイルは36,642バイト。
RescueのFAT32一致は1,587/152,325バイトで、残り22ファイルを分母から除いていない。
チェーンのない複数クラスタの配置を推測しない設計のため、22件は未対応候補として残る。
Carvingの結果を管理情報復元の結果へ混ぜていない。
元名・ルートからの相対パスが一致した復元物は、各製品とも全バイト一致件数と同数。
正解に一致する重複出力は0。選択時に含めたファイルシステムの管理ファイルと
Rescueのレポートは別計上し、分類できない出力は0だった。
これは全スキャン候補に対する誤検出率0の証明ではない。

## 製品・ライセンス・設定

- **Rescue 1.0.0**: Quick、管理情報復元、対応候補をすべて保存。通常の既定値。
  同じ入力で3回実行し、最初の実出力を品質採点に使用した。
- **UFS Explorer Standard Recovery 10.23 64 bit**: 無料評価版の256 KiB以下の復元機能。
  コーパス全36件は制限内。NTFS/FAT32は全イメージを対象FSだけでスキャン、
  RAW復元を無効にして認識結果のルートを保存した。
  exFATは認識済みパーティションの「ファイルシステムを索引化し未使用領域を探索」、
  既定のFS検出、RAW復元なしを使用した。最初のディスク全体のexFAT限定検索は
  結果を出さなかったため、その操作履歴もプロトコルへ残した。
- **DMDE 4.4.4 (2026) Free Edition x64**: 無料版のアクティブパネル内4,000件までの復元。
  全36件を同じルートに置き、読み取り専用で開いた。
  NTFSは全体検索の追加結果、FAT32は認識済みパーティションの削除一覧、
  exFATは「FSパラメーターを自動検出」後の全体検索結果を使用した。
  各FSだけを対象にし、RAW復元は無効。元ディレクトリを開いて削除ファイルを保存した。
- **R-Studio**: Linux版の起動時に管理者認証が必要だったため採点対象外。
  ダウンロードだけを比較の実施や失敗の証拠にしていない。

公式の配布元・利用条件:
[UFS Explorer](https://www.ufsexplorer.com/ufs-explorer-standard-recovery/)、
[DMDEの版と制限](https://dmde.com/manual/editions.html)、
[R-Studio Linux](https://www.r-studio.com/data_recovery_linux/Download.shtml)。
取得ファイルのSHA-256、各操作条件、実出力の全件採点は
[プロトコル](comparison-protocol-v1.json)と[結果JSON](comparison-results-v1.json)に記録した。

## 再現と測定範囲

[固定イメージ・正解・実出力・画面の証拠](comparison-evidence-v1.tar.gz)を展開すると、
`SHA256SUMS.txt`で全ファイルを確認できる。製品の実行ファイルやライセンスは同梱しない。
`scripts/score_comparison.py`は正解自体のハッシュ、入力の不変、実出力を再検査する。

```sh
python scripts/score_comparison.py --manifest comparison-evidence/manifest.json \
  --outputs comparison-evidence/outputs --product rescue-v1 --product ufs --product dmde \
  --output comparison-rechecked.json
```

新しく生成する場合は`make_comparison_fixture.py`を使う。
フォーマッターの時刻・IDが変わるので、比較途中に入力を再生成せず、最終像を全製品へ共有する。
今回は小さい固定イメージの品質比較であり、商用製品の速度・メモリの順位は測定していない。
実SSD/HDD、TRIM、4 KiBセクター、大ファイル、商用製品でのRAID・読取不良・
クイックフォーマットの比較には一般化しない。それらのRescue単独の対応・異常系は
各ネイティブ回帰試験で別に検証する。
