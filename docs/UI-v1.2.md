# v1.2.0-alpha.1の実画面

Linuxの実GUIをマウスとキーボードで操作し、同じビルドの画面を撮影した。[検証JSON](gui-v1.2-results.json)に実行バイナリ・出力・画像のSHA-256を保存している。Windowsの通常権限UIA/実キー/日本語IMEは別途ネイティブCIと公開実物で必須検証する。

## 取得と再開

![取得設定](images/v1.2-alpha.1/imaging-settings-ja.png)

所有する読取ヘルパーだけをSIGSTOPで停止し、1秒の待機上限を超えさせた。RAWには遅い結果を保存せず、4 MiBの失敗と60 MiBの未読を保持した。停止後は元ソースを示した再開ダイアログが開く。

![停止後の再開案内](images/v1.2-alpha.1/imaging-timeout-ja.png)

「取得を再開」を実際に押し、同じ元イメージから64 MiBすべてを取得。元ファイルとSHA-256が一致した。最初に失敗した4 MiBは逆方向の64 KiB単位で成功し、読取マップの試行回数は2となる。

![再開後の読取マップ](images/v1.2-alpha.1/imaging-map-ja.png)

## 復元とレポート

新規スキャン、検索、選択保持、アイコン切替、36ファイルのZIP復元、HTML/CSV保存、日英切替、720×520表示を実施。36ファイル・152,325バイトが元ハッシュと一致し、22件の連続推定ラベルを保持した。

![復元画面](images/v1.2-alpha.1/recovered-ja.png)

![HTMLレポート](images/v1.2-alpha.1/html-report.png)

[取得手順と制限](IMAGING.md)。ここでのプロセス停止は合成障害であり、物理USB/SDの切断や故障を測定したものではない。
