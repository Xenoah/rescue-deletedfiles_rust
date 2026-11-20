# Parallels HDS・スナップショットの実証

OpenVZ ploop 1.15が作成した旧形式のベースと、Linuxゲスト内で取得した2段のオフラインスナップショットを使います。各世代のXML、圧縮したメンバー、ゲスト側ログを収録しました。新形式では同じ世代の内容をQEMUで作成しています。RAWベースの構成ではploopが差分の種類にもPlainを記録するため、実データのHDSヘッダーを検証します。各メンバー、元のNTFS、世代ごとの論理ディスクのSHA-256は [oracle.json](oracle.json) に記録されています。

```sh
python scripts/e2e_parallels.py --binary /path/to/rescue
python scripts/e2e_parallels.py --binary /path/to/rescue --native
```

ポータブル試験は11通りで16 MiBの論理ディスク全体を原本と比較します。各出力から、NTFS3で実際に削除した同じ10ファイル（AVI、DOCX、FLAC、OGG、PPTX、SQLite、TAR、TIFF、WAV、XLSX）を削除前の割当記録で復元し、全バイトとSHA-256を照合します。拒否試験は51通りで、親の欠落、割当表の重複、読めない領域、別世代の同名ファイル、既存出力の保護を含みます。[ポータブル試験結果](../../docs/parallels-results.json)

Linux追加試験ではQEMUが展開した各世代とも照合します。別途FAT32・ext4にAAC、BMP、DOCX、FLAC、PDF、PNG、TAR、XLSXを書き込み、読み戻して削除します。それぞれをQEMUでHDSに変換し、RescueでRAWへ戻してからSleuth Kitで取り出した計16ファイルを原本と照合します。追加試験にはQEMU、dosfstools、mtools、e2fsprogs、Sleuth Kitが必要です。[Linux試験結果](../../docs/parallels-native-results.json)

v1.38.0の公開Linuxパッケージでは、自動判別による変換成功後もHDSや記述子そのものが出力されました。比較対象の論理ディスクは16 MiBです。[公開版での再現結果](public-v1.38-linux.json)
