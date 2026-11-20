#!/bin/sh
export PATH=/bin:/sbin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
exec </dev/console >/dev/console 2>&1
set -ex
trap 'echo GUEST_FAILURE; poweroff -f' EXIT
insmod /modules/virtio_blk.ko
insmod /modules/async_tx.ko
insmod /modules/raid6_pq.ko
insmod /modules/async_raid6_recov.ko
insmod /modules/md-mod.ko
insmod /modules/libcrc32c.ko
insmod /modules/xor.ko
insmod /modules/async_xor.ko
insmod /modules/async_pq.ko
insmod /modules/async_memcpy.ko
insmod /modules/raid456.ko

mdev -s
mkdir -p /dev/md /run/mdadm
[ -b /dev/md0 ] || mknod /dev/md0 b 9 0
uname -r
mdadm --version
mdadm --create /dev/md0 --run --force --metadata=1.0 --level=5 --raid-devices=4 --layout=left-symmetric --chunk=16 --size=4096 --bitmap=none /dev/vda /dev/vdb /dev/vdc /dev/vdd
mdadm --wait /dev/md0 || test "$(cat /sys/block/md0/md/sync_action)" = idle
cat /proc/mdstat
mdadm --detail /dev/md0
for i in a b c d; do cat /sys/block/md0/md/dev-vd$i/slot /sys/block/md0/md/dev-vd$i/offset; done
test "$(blockdev --getsize64 /dev/md0)" = 12582912
/sbin/mke2fs -q -F -t ext4 -b 1024 -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 /dev/md0 10240
mount -t ext4 -o nodiscard /dev/md0 /mnt
cp /orig/sample.* /mnt/
sync
for f in /orig/sample.*; do cmp "$f" "/mnt/${f##*/}"; done
sha256sum /mnt/sample.*
rm /mnt/sample.*
sync
for f in /orig/sample.*; do test ! -e "/mnt/${f##*/}"; done
umount /mnt
dd if=/marker of=/dev/md0 bs=1k seek=10617 conv=notrunc,fsync
blockdev --flushbufs /dev/md0
dd if=/dev/md0 of=/dev/vdf bs=1M conv=fsync
sha256sum /dev/md0 /dev/vdf
mdadm --stop /dev/md0
for d in /dev/vda /dev/vdb /dev/vdc /dev/vdd; do mdadm --examine "$d"; done
sync
echo GUEST_RAID6_OK
trap - EXIT
poweroff -f
