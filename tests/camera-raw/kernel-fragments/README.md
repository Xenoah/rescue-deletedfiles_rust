# 断片化したカメラRAWをLinuxカーネルで削除した試験

公開版v1.37.0のLinux配布物で、実カメラ16形式を断片化したNTFSとFAT32から復元しました。各原本を64 KiBずつ順に書き、書くたびに同期しました。マウント中に全ファイルを原本と比較して物理配置を記録し、カーネル経由で削除しました。各ボリュームは224 MiBです。[機種・原本・SHA-256](../sources.json) ｜ [ファイル別の照合結果と失敗例](public-v1.37-linux.json)

| ドライバー | 削除前に確認した物理範囲 | 保存した配置＋原本のSHA-256 | 4 KiBごとの指紋のみ |
| --- | --- | ---: | ---: |
| ntfs3（4 KiBクラスタ） | FIEMAPで計1,510区間、1本につき24～153区間 | 16/16一致 | 16/16一致 |
| vfat（FAT32、1 KiBクラスタ） | FIBMAPで計1,557区間、1本につき24～176区間 | 16/16一致 | 16/16一致 |

32件の削除を同じ2経路で検証しました。それぞれ出力の全バイトとSHA-256を照合し、両ディスクとも試験前後のSHA-256が一致しました。指紋による検索では各ボリュームで27,070チャンクを照合しました。ファイル名や削除前の物理アドレスを検索に渡す必要はありません。原本から事前に記録したチャンク単位の指紋は必要です。ファイル全体のSHA-256だけから断片の位置を特定することはできません。

ntfs3の削除後、内蔵とSleuth Kitのメタデータ検索はともに0件でした。FAT32の内蔵検索は16件を表示し、標準設定で復元可能と判定したものは0件でした。明示的な連続配置推定とSleuth Kitはそれぞれ16本を書き出しましたが、原本に一致したものはどちらも0本でした。サイズだけでは誤った復元を見抜けません。配置の逆順、別ディスクの配置、不正な原本SHA-256、破損したチャンク指紋は両ディスクで拒否され、出力は作成されませんでした。

## 再現

Linux 6.12.107の隔離VMで、それぞれ新規作成したボリュームイメージを使いました。実カメラ原本の取得手順は[親ディレクトリー](../README.md)にあります。次のコマンドはリポジトリ直下から、試験用の通常ファイルだけを対象に実行します。ゲスト内のマウントとFIBMAPには管理者権限が必要です。

```sh
set -eu
p=tests/camera-raw/kernel-fragments
work=$(mktemp -d)
mkdir "$work/mnt"
cc -O2 -Wall -Wextra -Werror "$p/write_interleaved.c" -o "$work/write_interleaved"
cc -O2 -Wall -Wextra -Werror "$p/capture_map.c" -o "$work/capture_map"
for fs in ntfs fat32; do
    img="$work/$fs.raw"
    truncate -s 224M "$img"
    case "$fs" in
        ntfs) mkntfs -F -Q -c 4096 "$img"; driver=ntfs3 ;;
        fat32) mkfs.fat -F 32 -s 2 "$img"; driver=vfat ;;
    esac
    mount -t "$driver" -o loop "$img" "$work/mnt"
    mkdir "$work/mnt/shoot"
    "$work/write_interleaved" "$work/mnt/shoot" camera-originals/camera.*
    sync
    for f in camera-originals/camera.*; do cmp "$f" "$work/mnt/shoot/${f##*/}"; done
    "$work/capture_map" "$work"/mnt/shoot/camera.* > "$work/$fs.map"
    rm "$work"/mnt/shoot/camera.*
    test -z "$(ls -A "$work/mnt/shoot")"
    sync
    umount "$work/mnt"
    sha256sum "$img"
done
python "$p/pack_layouts.py" --originals camera-originals --images "$work" --logs "$work" --output "$work/layouts.json.gz"
python scripts/verify_fragmented_cameras.py --binary /path/to/rescue --originals camera-originals --images "$work" --layouts "$work/layouts.json.gz" --output "$work/report.json"
```

再作成したイメージは削除時刻・配置などが異なるため、そのイメージで採取した配置を使用します。圧縮済みの [layouts.json.gz](layouts.json.gz) は公開した測定値の再点検用です。物理アドレスの起点はボリュームの先頭です。この試験ではイメージの先頭と一致します。

個別のファイルを復元する例です。配置指定には上記JSONの対象ボリューム内の `files["camera.dng"]` オブジェクトを保存して使用します。

```sh
rescue recover-extents --source fat32.raw --layout camera-dng-layout.json --output from-layout.dng --json
rescue fingerprint-file --source camera-originals/camera.dng --chunk-size 4096 --output camera-dng-fingerprint.json --json
rescue recover-fingerprinted --source fat32.raw --manifest camera-dng-fingerprint.json --alignment 512 --output from-fingerprint.dng --json
```
