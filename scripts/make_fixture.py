"""Deterministic, synthetic NTFS volume and independent expected byte strings.

Only writes a new image at the explicit output path. No real device operations.
"""
from pathlib import Path
import argparse
import hashlib
import json
import struct

SECTOR = 512
RECORD = 1024
MFT_LCN = 4
RECORDS = 64


def put(data, offset, fmt, value):
    struct.pack_into("<" + fmt, data, offset, value)


def resident(kind, value, flags=0):
    size = (24 + len(value) + 7) & ~7
    result = bytearray(size)
    put(result, 0, "I", kind)
    put(result, 4, "I", size)
    put(result, 12, "H", flags)
    put(result, 16, "I", len(value))
    put(result, 20, "H", 24)
    result[24:24 + len(value)] = value
    return result


def filename(name, parent=5, parent_sequence=1):
    encoded = name.encode("utf-16le")
    value = bytearray(66 + len(encoded))
    put(value, 0, "Q", parent | (parent_sequence << 48))
    value[64] = len(encoded) // 2
    value[65] = 1  # Win32 namespace
    value[66:] = encoded
    return resident(0x30, value)


def nonresident(runs, size, initialized=None, flags=0):
    # runs are (number of clusters, LCN or None); offsets are signed deltas.
    encoded = bytearray()
    previous = 0
    clusters = 0
    for count, lcn in runs:
        clusters += count
        length = count.to_bytes(max(1, (count.bit_length() + 7) // 8), "little")
        if lcn is None:
            offset = b""
        else:
            delta = lcn - previous
            width = next(n for n in range(1, 9) if -(1 << (n*8-1)) <= delta < (1 << (n*8-1)))
            offset = delta.to_bytes(width, "little", signed=True)
            previous = lcn
        encoded.append(len(offset) << 4 | len(length))
        encoded.extend(length + offset)
    encoded.append(0)
    attr = bytearray((64 + len(encoded) + 7) & ~7)
    put(attr, 0, "I", 0x80)
    put(attr, 4, "I", len(attr))
    attr[8] = 1
    put(attr, 12, "H", flags)
    put(attr, 24, "Q", clusters - 1)
    put(attr, 32, "H", 64)
    put(attr, 40, "Q", sum(n for n, lcn in runs if lcn is not None) * SECTOR)
    put(attr, 48, "Q", size)
    put(attr, 56, "Q", size if initialized is None else initialized)
    attr[64:64+len(encoded)] = encoded
    return attr


def record(number, attrs, flags=0, sequence=7):
    data = bytearray(RECORD)
    data[0:4] = b"FILE"
    put(data, 4, "H", 48)
    put(data, 6, "H", 3)
    put(data, 16, "H", sequence)
    put(data, 18, "H", 1)
    put(data, 20, "H", 56)
    put(data, 22, "H", flags)
    put(data, 28, "I", RECORD)
    put(data, 44, "I", number)
    cursor = 56
    for attr in attrs:
        data[cursor:cursor+len(attr)] = attr
        cursor += len(attr)
    assert cursor + 4 <= RECORD
    put(data, cursor, "I", 0xFFFFFFFF)
    put(data, 24, "I", (cursor + 4 + 7) & ~7)
    put(data, 48, "H", 0xA55A)
    for i in (1, 2):
        data[48+i*2:50+i*2] = data[i*SECTOR-2:i*SECTOR]
        put(data, i*SECTOR-2, "H", 0xA55A)
    return data


def build(path, corrupt=False, unsupported=False, records=RECORDS):
    size = max(1024*1024, (MFT_LCN * SECTOR + records * RECORD + 4095) & ~4095)
    volume = bytearray(size)
    volume[0:3] = b"\xeb\x52\x90"
    volume[3:11] = b"NTFS    "
    put(volume, 11, "H", SECTOR)
    volume[13] = 1
    put(volume, 40, "Q", size // SECTOR - 1)
    put(volume, 48, "Q", MFT_LCN)
    volume[64] = 246  # -10 => 1024-byte FILE record
    put(volume, 72, "Q", 0x1020304050607080)
    volume[510:512] = b"\x55\xaa"
    volume[-SECTOR:] = volume[:SECTOR]
    entries = {
        0: record(0, [nonresident([(records*2, MFT_LCN)], records*RECORD)], flags=1, sequence=1),
        5: record(5, [filename(".")], flags=3, sequence=1),
        10: record(10, [filename("写真")], flags=3, sequence=2),
    }
    expected = {}

    def deleted(number, name, data, attr=None, parent=5, parent_sequence=1):
        entries[number] = record(number, [
            filename(name, parent, parent_sequence),
            resident(0x80, data) if attr is None else attr,
        ])
        expected[f"mft-{number}-7"] = {
            "name": name, "hex": data.hex(), "size": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }

    deleted(20, "日本語.txt", "元のファイルをバイト一致で復元。\n".encode(), parent=10, parent_sequence=2)
    fragment = b"A"*512 + b"B"*512 + b"C"*76 + b"\0"*200
    deleted(21, "fragmented.bin", fragment,
            nonresident([(1, 300), (1, 302), (1, 301)], len(fragment), initialized=1100))
    volume[300*SECTOR:301*SECTOR] = b"A"*512
    volume[302*SECTOR:303*SECTOR] = b"B"*512
    volume[301*SECTOR:302*SECTOR] = b"C"*512
    sparse = b"L"*512 + b"\0"*512 + b"R"*512
    deleted(22, "sparse.bin", sparse, nonresident([(1, 310), (1, None), (1, 312)], len(sparse), flags=0x8000))
    volume[310*SECTOR:311*SECTOR] = b"L"*512
    volume[312*SECTOR:313*SECTOR] = b"R"*512
    deleted(23, "empty.txt", b"")
    deleted(24, "../../CON:escape.txt", b"must stay inside the destination")
    deleted(25, "stale-parent.txt", b"parent record has been reused", parent=10, parent_sequence=99)
    entries[26] = record(26, [filename("live.txt"), resident(0x80, b"still live")], flags=1)
    if corrupt:
        entries[27] = record(27, [filename("torn.txt"), resident(0x80, b"bad")])
        entries[27][510] ^= 1
    if unsupported:
        entries[28] = record(28, [filename("compressed.bin"), nonresident([(1, 320)], 512, flags=1)])
        entries[29] = record(29, [filename("extent-list.bin"), resident(0x20, b""), resident(0x80, b"do not restore")])
    for number, content in entries.items():
        start = MFT_LCN * SECTOR + number * RECORD
        volume[start:start+RECORD] = content
    path = Path(path)
    with path.open("xb") as f:
        f.write(volume)
    return expected


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--corrupt", action="store_true")
    parser.add_argument("--unsupported", action="store_true")
    args = parser.parse_args()
    result = build(args.output, corrupt=args.corrupt, unsupported=args.unsupported)
    print(json.dumps(result, ensure_ascii=False, indent=2))
