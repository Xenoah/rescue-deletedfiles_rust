# Linuxで削除したexFATカメラカード

公開版Rescue v1.37.0のLinux版で、16形式・計110,844,883バイトの実カメラ原本を2枚のexFATディスクイメージから復元しました。どちらもLinux 6.12.107のexFATドライバーで書き込み、全バイトを読み戻してから削除しています。削除前の物理配置はLinuxのFIBMAPで採取し、削除後のディスクから各区間を直接読んで原本と照合しました。[原本と機種](../sources.json) ｜ [全ファイルのハッシュと破損試験](public-v1.37-linux.json) ｜ [ゲストの記録](capture.txt.gz)

| 書き込み方 | 削除前の物理配置 | 内蔵の標準復元 | 外部Sleuth Kit 4.12.1 |
| --- | --- | ---: | ---: |
| 1本ずつコピー | 各ファイル1区間 | 16/16一致 | 16/16一致 |
| 全ファイルを64 KiBずつ交互に書き込み、毎回同期 | 計1,557区間、各24～176区間 | 16/16一致 | 0/16一致 |

内蔵復元には事前の配置やハッシュを渡していません。連続配置ではexFATのストリームフラグ、断片化したディスクでは削除後に残ったFATチェーンを使用しました。ファイル名・パスと32件すべての全バイト・SHA-256が原本に一致し、元ディスクのSHA-256も試験前後で一致しました。断片化したカードでSleuth Kitが保存した16本はすべて正しいサイズでしたが、内容はすべて異なりました。内蔵のexFAT復元にはSleuth Kitは不要です。

破損試験は削除後のディスクのコピーで行いました。対象DNGの先頭クラスタの割当ビットを立てると両カードで復元を拒否し、断片化側で先頭のFATリンクをクリアした場合と自己参照にした場合も拒否しました。同じファイルの解放済みデータを1バイトだけ変え、管理情報を残すと、両カードで書き出されました。この破損は原本のSHA-256との照合で検出できました。元のデータが上書きされたあとの正しさは、管理情報だけでは確認できません。

## 再現手順

隔離したLinux VMで、新規作成した通常ファイルにexFATを作成します。原本は[カメラRAW試験の手順](../README.md)で準備します。試験用ヘルパーは[断片化試験](../kernel-fragments/README.md)と共通です。ゲスト内のマウントと配置記録には管理者権限が必要です。結果の比較にはPython 3.11以降とSleuth Kitを使用します。

```sh
set -eu
work=$(mktemp -d)
mkdir "$work/mnt"
helpers=tests/camera-raw/kernel-fragments
cc -O2 "$helpers/capture_map.c" -o "$work/capture"
cc -O2 "$helpers/write_interleaved.c" -o "$work/interleave"
for kind in contiguous fragmented; do
    img="$work/$kind.raw"
    truncate -s 224M "$img"
    mkfs.exfat -c 4096 "$img"
    mount -t exfat -o loop "$img" "$work/mnt"
    shoot="$work/mnt/shoot"
    mkdir "$shoot"
    if [ "$kind" = fragmented ]; then
        "$work/interleave" "$shoot" camera-originals/camera.*
    else
        cp camera-originals/camera.* "$shoot/"
    fi
    sync
    for f in camera-originals/camera.*; do cmp "$f" "$shoot/${f##*/}"; done
    echo "FRAG_START_$kind" >> "$work/guest.log"
    "$work/capture" "$shoot"/camera.* >> "$work/guest.log"
    echo "FRAG_END_$kind" >> "$work/guest.log"
    rm "$shoot"/camera.*
    test -z "$(ls -A "$shoot")"
    sync
    umount "$work/mnt"
    echo "VOLUME_SHA256 $kind $(sha256sum "$img" | cut -d' ' -f1)" >> "$work/guest.log"
done
echo CAMERA_KERNEL_OK >> "$work/guest.log"
python scripts/verify_exfat_camera_kernel.py --binary /path/to/rescue --originals camera-originals --images "$work" --guest-log "$work/guest.log" --output "$work/report.json"
```
