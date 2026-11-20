#!/bin/sh
export PATH=/bin:/sbin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
exec </dev/console >/dev/console 2>&1
set -ex
trap 'echo GUEST_FAILURE; poweroff -f' EXIT
insmod /modules/virtio_blk.ko
insmod /modules/fat.ko
insmod /modules/vfat.ko
insmod /modules/nls_cp437.ko
insmod /modules/nls_ascii.ko
insmod /modules/nls_utf8.ko

mdev -s
uname -r
for drive in a b; do
  if [ "$drive" = a ]; then mount -t vfat /dev/vda /mnt; else mount -t ext4 -o nodiscard /dev/vdb /mnt; fi
  cp /models/*.glb /mnt/
  sync
  for file in /models/*.glb; do cmp "$file" "/mnt/${file##*/}"; done
  sha256sum /mnt/*.glb
  rm /mnt/*.glb
  sync
  for file in /models/*.glb; do test ! -e "/mnt/${file##*/}"; done
  umount /mnt
  sha256sum /dev/vd$drive
done
echo KERNEL_GLB_UNLINK_OK
trap - EXIT
poweroff -f
