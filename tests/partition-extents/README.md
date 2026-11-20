# パーティション先頭を起点とするファイル配置

NTFSとFAT32でAVI、DOCX、FLAC、OGG、PPTX、SQLite、TAR、TIFF、WAV、XLSXの10形式を実際に削除しました。原本はリポジトリの [tests/corpus/files](../corpus/files) にあります。2つの圧縮RAWはいずれも削除後の状態です。

Linux 6.12.107の隔離VMで、ntfs3（16 MiB、1 KiBクラスタ）とvfat（48 MiB、512バイトクラスタ）をマウントしました。上記の10ファイルに1 KiBずつ交互に書き込んで毎回同期した後、全ファイルを原本と比較しました。削除前にNTFSはFIEMAP、FAT32はFIBMAPで物理配置を記録し、それぞれ10本を `rm` で削除しました。空ディレクトリを確認してアンマウントし、ゲスト内外のディスク全体のSHA-256を照合しました。さらに、削除後のディスクから各区間を読み直して原本と一致することを確認しました。各ファイルは4～29区間に分かれています。[削除前の配置と原本ハッシュ](layouts.json) ｜ [ゲストの実行記録](capture.txt.gz)

`scripts/e2e_partition_extents.py` は、NTFSを4 KiBセクタのGPTの第2パーティション、FAT32をMBR拡張領域内の論理パーティション5に配置します。削除前のボリューム相対位置をそのまま形式2の配置表に指定し、20件すべての全バイトとSHA-256を原本と照合します。逆順の配置20件では保存を拒否します。移植可能な8形式の試験では、MBR基本・論理、GPT（512バイト・4 KiB）、Apple Partition Mapの5通りについて、スパース領域、旧形式の互換性、パーティション境界、取得マップ上の欠損も検証します。

```sh
python scripts/e2e_partition_extents.py --binary /path/to/rescue
```

ゲスト試験を再作成するには、[カメラRAW断片化試験](../camera-raw/kernel-fragments/README.md)のヘルパーを使用し、`write_interleaved.c` のコンパイル時に `-DWRITE_CHUNK_BYTES=1024` を渡します。ここでは対象の10原本のみを用意し、NTFSは `mkntfs -F -Q -c 1024`、FAT32は `mkfs.fat -F 32 -s 1` で作成しました。再作成時は実際の配置とハッシュを採取し直す必要があります。

## 実カメラ原本を含むカード

別に、Linuxカーネルで削除した16形式のカメラRAWを含む4枚のカードをGNU Parted 3.6で作成したディスクに組み込みました。内訳はNTFSをGPT、FAT32をMBR論理パーティション、連続配置のexFATをApple Partition Map、断片化したexFATをMBR基本パーティションに配置したものです。64件すべてが削除前の物理配置から全バイト・SHA-256とも一致しました。[4枚のディスクと全ファイルの測定値](camera-results.json) ｜ [原本と機種](../camera-raw/sources.json) ｜ [検証スクリプト](../../scripts/verify_partition_camera_cards.py)
