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
mount -t f2fs -o nodiscard,noinline_data,background_gc=off /dev/vda /mnt
fragfiles /models/original.*
n=0
for f in /models/original.*; do n=$((n+1)); cmp "$f" "/mnt/sample-$n.bin"; done
sha256sum /mnt/sample-*.bin
rm /mnt/sample-*.bin
sync
umount /mnt
mount -t f2fs -o ro,norecovery /dev/vda /mnt
for n in 1 2 3 4 5 6; do test ! -e "/mnt/sample-$n.bin"; done
umount /mnt
sha256sum /dev/vda
echo KERNEL_F2FS_UNLINK_OK
trap - EXIT
poweroff -f
