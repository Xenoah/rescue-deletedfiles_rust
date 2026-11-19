//! CRC32C with an explicit initial state, as used by ext4, JBD2 and Btrfs.
const fn crc_table() -> [u32; 256] {
    let mut a = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u32;
        let mut j = 0;
        while j < 8 {
            v = (v >> 1) ^ if v & 1 != 0 { 0x82f63b78 } else { 0 };
            j += 1;
        }
        a[i] = v;
        i += 1;
    }
    a
}
const CRC: [u32; 256] = crc_table();
pub(crate) fn update(mut c: u32, data: &[u8]) -> u32 {
    for &b in data {
        c = (c >> 8) ^ CRC[usize::from(c as u8 ^ b)];
    }
    c
}
