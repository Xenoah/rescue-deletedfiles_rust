# Rescue v1.26.0

QEMUのQCOW2 v3で、Zstandard圧縮クラスタと拡張L2テーブルを使った仮想ディスクを直接読み込めるようになりました。CLIまたはGUIで単独のQCOW2を開き、新しいRAWに書き出してスキャンできます。取り込みにQEMUのインストールは不要です。

```sh
rescue import-image --source guest.qcow2 --output guest.raw --json
rescue scan --source guest.raw --work-dir scan --json
```

拡張L2テーブルでは、各クラスタに含まれる32個のサブクラスタについて、どこに有効なデータがあるかを判定します。ゼロ指定または未割り当ての部分は、ホスト側に古いデータが残っていてもゲストにはゼロとして書き出します。Zstandardフレームは展開後のクラスタ長と、フレームに記録されたチェックサムを確認します。

## 実ディスクイメージでの照合

| 元のディスク | QEMUでの取得条件 | 結果 |
| --- | --- | --- |
| FAT32 | Zstandard、拡張L2、16 KiBクラスタ | BMP・GIF・PNG・PDF・WebP・WAV・ZIPの実削除7件を復元 |
| ext4 | Deflate、拡張L2、64 KiBクラスタ | 同じ7形式の実削除7件を復元 |
| Windows NTFS | Zstandard、標準L2、64 KiBクラスタ | Windowsで削除したHTML・JSON・RTF・XML・PNG・ZIP・gzipの7件を元の名前で復元 |

3ディスクの全バイトをQEMUの独立した展開結果と照合し、復元した21件も原本のバイト列とSHA-256がすべて一致しました。FAT32とext4では、ゲストがゼロとして扱うサブクラスタのホスト領域に意図的に0xa9を書き込み、その値がRAWに混入しないことも確認しました。不正な割り当て、圧縮フラグ、Zstandardフレーム長・チェックサムなど15条件で出力を拒否しました。[実測値と原本ハッシュ](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.26.0/docs/qcow2-subclusters-v1.26-results.json)

Linuxの実GUIから3種類のQCOW2を変換し、自動選択された各RAWからCLIで上記の7件ずつを再照合しました。GUIも未知の圧縮方式を拒否しました。ローカルのRustテスト96件・統合テスト83組が通過しました。35形式・36原本の比較試験は1,116件中1,016件が原本と完全一致し、欠損や上書きを含む条件ごとの期待値に一致しました。[GUI検証](gui-v1.26-results.json)

GNU ddrescueの検証には、WindowsのMBR・512バイトセクターとGPT・4 KiBセクターの部分取得を追加しました。元のパーティション表が取得範囲外でも、保存位置を前後にずらした4条件でNTFSを再発見し、各7件の実削除ファイルを復元しました。[部分取得の照合記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.26.0/docs/ddrescue-shifted-v1.25-results.json)

## 復元範囲と外部ツール

対象は親イメージのないQCOW2 v2・v3です。拡張L2とZstandardはv3で扱います。外部データファイル、バックチェーン、内部スナップショット、暗号化、dirty・corruptフラグは取り込みを拒否します。バックチェーンの復元には親の実ファイル、暗号化の場合には鍵と対応ツールが必要です。上書きやTRIMで元バイトが失われた場合は、別のコピーが必要です。

実証の再現にはQEMU・mtools・e2fsprogsを使用します。GNU ddrescueの部分取得比較にはGNU ddrescue・ddrescuelogとVHDXを読めるThe Sleuth Kitを使用します。[操作と制限](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.26.0/docs/IMAGING.md)
