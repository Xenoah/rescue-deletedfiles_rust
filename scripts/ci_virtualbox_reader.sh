#!/usr/bin/env bash
# Extract the user-space reference reader, without installing a hypervisor or kernel modules.
set -euo pipefail
reader_dir="$RUNNER_TEMP/rescue-vdi-reader"
mkdir -p "$reader_dir"
cd "$reader_dir"
package='virtualbox_6.1.50-dfsg-1~ubuntu1.22.04.3_amd64.deb'
relative="pool/multiverse/v/virtualbox/$package"
if ! curl --fail --location --retry 2 --max-time 120 --output "$package" "https://archive.ubuntu.com/ubuntu/$relative"; then
  curl --fail --location --retry 2 --max-time 120 --output "$package" "https://mirrors.edge.kernel.org/ubuntu/$relative"
fi
echo "dc3ee24e04420d80ef9c39212f3189707d7a97ad1b9cbc40d018436c4ceb03d9  $package" | sha256sum -c -
dpkg-deb --fsys-tarfile "$package" | tar -x ./usr/lib/virtualbox/{VBoxRT.so,VBoxDDU.so,vbox-img}
echo "RESCUE_TEST_VBOX_IMG=$reader_dir/usr/lib/virtualbox/vbox-img" >> "$GITHUB_ENV"
