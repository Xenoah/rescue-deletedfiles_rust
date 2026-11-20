# DNG・CinemaDNGの連続領域からの復元

クラシックTIFFに格納された単一のフル解像度Bayerセンサー画像を扱います。ルート、SubIFD、Exif、GPS、Interopの各ディレクトリーを上限付きでたどり、タグ値・ストリップ・タイルの範囲、重なり、画素数を検証します。非圧縮の8・10・12・14・16ビットと、Huffman符号を使うロスレスJPEG（SOF3）に対応します。圧縮データは、全サンプルの差分符号・再開マーカー・終端まで確認します。RGBの非圧縮プレビューとJPEGプレビューも対象です。JPEGプレビューは通常のJPEG検証器で画素をデコードします。

Rescueのスキャンと復元にカメラ用の外部デコーダーは不要です。`rescue scan --source disk.raw --mode carve --extension dng --work-dir scan --json` で検索できます。断片化した写真、LinearRaw、Lossy DNG、JPEG XL、未知のプライベートタグ・MakerNoteはこの方式の対象外です。構造だけで確定できるのは最後に参照されたバイトまでです。未参照の末尾データ、原本の名前、削除されたこと、画素の元の値は別途確認が必要です。

## 実証データ

- [センサー原本15本](manifest.json) は、4種類のBayer配列、大小両エンディアン、ストリップ・タイル、行末に余りビットがある10・12・14ビットを含みます。tifffile、NumPy、imagecodecsのliblj92とlibjpegによるSOF3で生成しました。15本それぞれをFAT32とext4に書き込んで読み戻し、mtoolsまたはdebugfsで削除しました。通常・索引検索で延べ30件の全バイトとSHA-256が原本と一致しました。LibRawで展開した全センサー値を生成前の16ビット配列とも照合しました。[実証記録](../../docs/dng-native-results.json)
- [カメラ原本10本](cameras.json) はraw.pixls.usがCC0で公開した撮影データです。Linux 6.12.107のVMで各ファイルを120 MiBのNTFS・ext4に書き、同期・読み戻し後に `rm` で削除しました。NTFSの通常一覧は0件でした。連続領域からCanonの6本を完全復元し、Blackmagicの2本はそれぞれ元の末尾1バイト・44バイトを除く範囲を復元しました。この2本もLibRawで展開したセンサー配列は原本と一致しました。残り2本は対応外でした。ext4では現在のinodeから候補が得られず、過去のジャーナルの名前と配置から10本とも原本全体と一致しました。[機種別・復元方法別の結果](../../docs/dng-camera-results.json)
- 壊れたIFD・タグ型・重なったストリップ・過大な画像寸法・途切れたJPEG符号・取得できなかったセクターを使った異常系も確認しました。構造が整ったまま実際の画素値だけが変わった場合は復元でき、`match-hashes` で既知の原本との不一致を確認できます。[異常系の記録](../../docs/dng-results.json)

## 再現手順

小型原本はリポジトリに同梱しています。生成し直す場合はNumPy、tifffile、imagecodecsを使用します。Linuxの削除実証はdosfstools、mtools、e2fsprogs、Sleuth Kit、libraw-binを使用します。

```sh
python scripts/e2e_dng.py --binary target/release/rescue
python scripts/e2e_dng.py --native --binary target/release/rescue
```

撮影データで再検証するには、原本一覧の各URLから指定名で保存してください。専用の試験ディスク2枚に各原本をコピーし、同期して全ファイルを読み戻し、削除して再び同期し、アンマウントした時点のRAWを使用します。記録したディスクはLinuxのntfs3とext4で作成し、どちらも4 KiBブロック、ext4はdiscard無効です。

```sh
python scripts/verify_dng_cameras.py --binary target/release/rescue --originals camera-originals --ntfs ntfs.raw --ext4 ext4.raw --output camera-results.json
```
