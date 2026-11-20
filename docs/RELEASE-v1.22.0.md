# Rescue v1.22.0

Android sparse 1.0のイメージを外部ツールなしで読み、RAWと読取マップを新しいフォルダーに保存できます。RAWチャンクとFILLチャンクに記録された内容を復元し、DONT_CAREで省略されたバイトを未取得として保持します。

```sh
rescue import-android --source system.img --destination system-import --json
rescue read-map --image system-import/image.rescue.raw --json
rescue scan --source system-import/image.rescue.raw --mode carve --work-dir scan --json
```

GUIでは「イメージをRAWに変換」で「Android sparse」を選びます。変換後は読取マップ付きRAWが復元元に設定されます。チェックサムの不一致や変換失敗、取消時には完了したフォルダーを公開しません。

## 実ファイルでの検証

| イメージ | 実際の削除と照合 |
| --- | --- |
| FAT32、1 KiBブロック | mtoolsで7形式を実際に削除。AOSPの変換結果と論理RAWの全バイトが一致し、7件すべてを原本と照合 |
| ext4、4 KiBブロック | e2fsprogsで同じ7形式を削除。AOSPの論理RAWとの全バイト照合、7件の復元と原本照合に成功 |
| FAT32、4 KiBブロック、未取得範囲あり | 41,263,104バイトを未取得として区別し、記録された領域から削除済み7件の全バイトとSHA-256が一致 |
| 保存したAOSP参照イメージ | 実削除後に `img2simg -s` が作成したイメージを使い、通常検索と索引検索で7/7件ずつ一致 |
| デスクトップGUI | 3種類の入力を変換し、選択されたスキャン元・読取マップ・各7件の内容を照合。完全に記録されたFAT32ではGUIからも7件を選びZIPに復元 |

対象はBMP、GIF、PNG、PDF、WebP、WAV、ZIPです。記録されたゼロと未取得領域の区別、ヘッダー・中間・末尾のCRC-32、破損した19例と同時保存時の動作も確認しました。[原本・再現手順](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/v1.22.0/tests/android-sparse) · [GUIの照合結果と画像](gui-v1.22-results.json)

未取得の領域、上書きされた内容、暗号化ボリュームの平文は、この変換では復元できません。実機のAndroid端末やフラッシュメモリーからの直接取得は今回の試験範囲外です。複数のfastbootイメージの合成や動的パーティションの展開は、それぞれ対応するAndroidツールを使います。[対応範囲・上限](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.22.0/docs/IMAGING.md#android-sparseイメージ)

原本のサイズとSHA-256が残った場合の35形式・36原本の比較結果も収録しました。管理情報消去後の連続配置で36/36件、1セクターおきに断片化した配置で先頭位置不明なら14/36件、先頭位置既知なら16/36件、全領域の上書き後は0/36件です。[ファイル別の結果と再現手順](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.22.0/docs/WHOLE-HASH-CORPUS.md)
