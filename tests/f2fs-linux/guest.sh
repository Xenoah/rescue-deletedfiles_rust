#!/bin/sh
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
exec </dev/console >/dev/console 2>&1
set -ex
trap 'echo GUEST_FAILURE; poweroff -f' EXIT
insmod /modules/virtio_blk.ko
insmod /modules/crc32_generic.ko
insmod /modules/crc32c_generic.ko
insmod /modules/lz4hc_compress.ko
insmod /modules/lz4_compress.ko
insmod /modules/f2fs.ko

mdev -s
uname -r
for disk in a b c; do
  case "$disk" in
    b) opts=nodiscard,inline_data,background_gc=off ;;
    *) opts=nodiscard,noinline_data,background_gc=off ;;
  esac
  mount -t f2fs -o "$opts" /dev/vd$disk /mnt
  cat /proc/mounts
  cp /models/sample.* /mnt/
  cp /retained.png /mnt/retained.png
  sync
  for f in /models/sample.*; do cmp "$f" "/mnt/${f##*/}"; done
  sha256sum /mnt/sample.* /mnt/retained.png
  rm /mnt/sample.*
  sync
  for f in /models/sample.*; do test ! -e "/mnt/${f##*/}"; done
  cmp /mnt/retained.png /retained.png
  if [ "$disk" = c ]; then cat /sys/block/vdc/queue/discard_max_bytes; fstrim -v /mnt; sync; fi
  umount /mnt
  mount -t f2fs -o ro,norecovery /dev/vd$disk /mnt
  cmp /retained.png /mnt/retained.png
  for f in /models/sample.*; do test ! -e "/mnt/${f##*/}"; done
  umount /mnt
  sha256sum /dev/vd$disk
done
echo KERNEL_F2FS_UNLINK_OK
trap - EXIT
poweroff -f
