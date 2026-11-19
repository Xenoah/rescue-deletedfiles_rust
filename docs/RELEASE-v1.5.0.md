# Rescue v1.5.0

QCOW2を内蔵リーダーでRAWに展開できるようになりました。GUI・CLI・MCPのカービングにはアニメーションPNG、Windowsアイコン、カーソルを追加しました。CLIでは、PhotoRecを使ってさらに多くの形式を探せます。

## 仮想ディスクとRAW画像

```sh
rescue import-image --source guest.qcow2 --output guest.raw --json
rescue scan --source guest.raw --mode carve --work-dir sessions --indexed --json
rescue photorec --source guest.raw --destination recovered --json
```

QCOW2 v2・v3の単独ディスクに対応し、通常のクラスタ、未割当・ゼロクラスタ、標準DEFLATE圧縮クラスタを読み取ります。クラスタサイズは512バイトから2 MiBです。L1・L2テーブル、参照カウント表・ブロックの位置、メタデータとデータの重なり、ファイル範囲を検査します。親ディスク、内部スナップショット、暗号化、外部データ、拡張L2、dirty/corruptフラグ付きのイメージは取り込み対象外です。イメージ内のパスを開く処理はありません。展開を完了した時点で、新規RAWのバイト数とSHA-256を表示します。

PhotoRecは別途インストールします。探索の範囲はディスク全体です。`--all-families` で希少形式を含めて検索できます。既定設定と全形式指定では同じ原本に対する結果が異なることがあります。[使い方と照合結果](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.5.0/docs/PHOTOREC.md)

## 新しいファイル形式

| 形式 | 復元時に確認する内容 |
| --- | --- |
| APNG | 全フレームのチャンクCRC、矩形・連番、各zlibストリームの終端、復号した画素。独立した表紙、部分フレーム、パレット、Adam7に対応。最大4,096フレーム、フィルター付き画素列は合計128 MiB |
| ICO・CUR | 格納されている全画像の位置、寸法、サイズ、カーソルのホットスポット。静止PNG、非圧縮の40バイトDIBヘッダーとANDマスクに対応。1ディレクトリ256画像まで。重複・隙間のある配置は対象外 |

APNGは `.apng` として保存します。`--extension png` でも検索できます。静止画のプレビューと、アニメーション全体を復元する処理は別機能です。

## 実証

- qemu-imgで作成したQCOW2 v2、v3、DEFLATE圧縮、4 KiBクラスタ、2 MiBクラスタの5条件で論理RAW全体が元の16 MiBと一致しました。各条件からext4上で削除した7ファイルを復元し、計35件で元バイト列とSHA-256が一致しました。
- 別実装で生成したQCOW2 v2とv3でも、逆順の配置、未割当、圧縮、端数の最終ブロックを含むRAWと、AVI・MKV・WebM・Ogg・gzipの5ファイルが一致しました。親参照、破損圧縮、範囲外の参照、管理領域の重複など11条件では出力を公開しませんでした。
- APNGの7パターン、ICO・CURの7パターンを通常スキャンと索引スキャンの両方で復元し、28件すべてで原本と一致しました。破損したAPNGの6条件とICO・CURの6条件を拒否しました。
- NTFS管理情報を消した35形式・36原本では、連続配置で23/36件、断片化配置で6/36件が原本と一致しました。この数値は固定コーパスの結果です。
- GUIから7件のAPNGを選択してZIPに復元し、全ファイル、HTML・CSVレポートを照合しました。読取中断からの再開、CP932、破損exFAT、既知ハッシュ照合も記録しています。[GUI実操作の記録](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/v1.5.0/docs/gui-v1.5-results.json)
