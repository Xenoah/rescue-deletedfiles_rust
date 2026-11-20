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
mdadm --assemble --run /dev/md0 /dev/vda /dev/vdb /dev/vdc /dev/vdd /dev/vde
mdadm --detail /dev/md0
mdadm /dev/md0 --fail /dev/vda --remove /dev/vda
test "$(cat /sys/block/md0/md/degraded)" = 1
dd if=/marker of=/dev/md0 bs=4096 count=4 seek=2600 conv=notrunc,fsync
blockdev --flushbufs /dev/md0
dd if=/dev/md0 of=/dev/vdf bs=1M conv=fsync
sha256sum /dev/md0 /dev/vdf
mdadm --detail /dev/md0
mdadm --stop /dev/md0
for d in /dev/vdb /dev/vdc /dev/vdd /dev/vde; do mdadm --examine "$d"; done
echo GUEST_GENERATION_OK
trap - EXIT
poweroff -f
