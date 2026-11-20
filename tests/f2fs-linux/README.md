# F2FSで実際に削除した10形式を公開版で復元する

公開v1.46.0のLinux x64ポータブル版を使い、F2FSのイメージを「内容から検索」でスキャンしました。BMP・FLAC・GIF・JPEG・PDF・PNG・SQLite・WAV・WebP・ZIPを、削除前の原本と全バイト・SHA-256で比較しました。Linux 6.12.107の隔離VMで各128 MiBのディスクをマウントし、10原本を書き込み、読み戻して比較し、通常の `rm` で削除しました。各ディスクに内容の異なるPNGを1件残し、アンマウント後の読み取り専用再マウントでも残存と削除を確認しています。ファイルシステム作成にはf2fs-tools 1.16.0を使用しました。

| Linuxでの削除条件 | 通常検索で原本と一致 | 索引検索で原本と一致 | 残したPNG |
| --- | ---: | ---: | --- |
| `nodiscard,noinline_data` | 10/10 | 10/10 | 一致 |
| `nodiscard,inline_data` | 10/10 | 10/10 | 一致 |
| `nodiscard,noinline_data` で削除後、明示的に `fstrim -v` | 0/10 | 0/10 | 一致 |

3番目のゲストでLinuxは117,424,128バイトをTRIMしたと報告しました。QEMUのvirtioディスクはdiscardをホストのRAWファイルに伝えます。TRIM後の全128 MiBを調べても削除した10原本の完全な連続バイト列は残っていませんでした。この結果はこの構成の挙動です。物理SSD・スマートフォンでのTRIM後の挙動、Androidの暗号化や個々のファイルの断片化は別の条件になります。失われた領域は、削除前のバックアップや別コピーが必要です。

ここで実証したのはファイル内容からの復元です。残しておいたPNGも3ディスクすべてで見つかりました。検索結果に出たことだけで削除の有無や元のファイル名は判定できません。`inline_data` はマウント時の設定であり、10ファイルすべてがインライン格納されたという意味ではありません。復元にはf2fs-toolsが不要です。

```sh
xz -dc tests/f2fs-linux/noinline.raw.xz > noinline.raw
rescue scan --source noinline.raw --mode carve --extension bmp,flac,gif,jpg,pdf,png,sqlite,wav,webp,zip --work-dir scan --json
python scripts/verify_f2fs_unlinks.py --binary /path/to/rescue --output f2fs-result.json
```

ポータブル版はリリースの `SHA256SUMS.txt` と照合し、中の実行ファイルも照合しました。公開版とv1.55.0候補は、同じ3ディスクで同じ10/10・10/10・0/10になりました。[公開パッケージ](https://github.com/Xenoah/rescue-deletedfiles_rust/releases/tag/v1.46.0) · [公開版のファイル別結果とパッケージSHA-256](published-v1.46-linux.json) · [候補版のファイル別結果](candidate-v1.55-linux.json) · [原本と各ディスクのSHA-256](manifest.json) · [ゲストのコマンド](guest.sh) · [Linuxの実行ログ](guest.txt.gz)

断片化した別ディスクでは、通常・索引の内容検索は6原本の完全復元が0件でした。削除前の配置またはブロックごとの照合情報を使えば6件とも復元できました。[6形式・179範囲の実測](fragmented)
