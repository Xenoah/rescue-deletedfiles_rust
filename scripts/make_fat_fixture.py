"""Deterministic FAT32/exFAT RAW volumes; writes only a new ordinary image file."""
import hashlib
import struct
from pathlib import Path


def put(data, offset, fmt, value):
    struct.pack_into("<"+fmt, data, offset, value)


def rotate_sum(data, width):
    result = 0
    mask = (1 << width)-1
    for byte in data:
        result = (((result >> 1) | ((result & 1) << (width-1))) + byte) & mask
    return result


def sfn(name, cluster, size, deleted=True, directory=False):
    assert len(name) == 11
    entry = bytearray(32)
    entry[:11] = name
    entry[11] = 0x10 if directory else 0x20
    put(entry, 20, "H", cluster >> 16)
    put(entry, 26, "H", cluster & 65535)
    put(entry, 28, "I", size)
    if deleted:
        entry[0] = 0xE5
    return entry


def lfn(name, short_name, deleted=True):
    codes = list(struct.unpack("<"+"H"*(len(name.encode("utf-16le"))//2), name.encode("utf-16le")))
    if len(codes) % 13:
        codes += [0] + [0xFFFF] * ((-len(codes)-1) % 13)
    result = []
    count = len(codes)//13
    checksum = rotate_sum(short_name, 8)
    for index in range(count, 0, -1):
        entry = bytearray(32)
        entry[0] = 0xE5 if deleted else index | (0x40 if index == count else 0)
        entry[11] = 15
        entry[13] = checksum
        for offset, code in zip((1,3,5,7,9,14,16,18,20,22,24,28,30), codes[(index-1)*13:index*13]):
            put(entry, offset, "H", code)
        result.append(entry)
    return result


def fat32(path, deleted_folder=False):
    sector, cluster, reserved, count, fat_sectors = 512, 512, 32, 65536, 513
    heap = (reserved+2*fat_sectors)*sector
    total = heap+count*cluster
    boot = bytearray(sector)
    boot[:3] = b"\xeb\x58\x90"
    boot[3:11] = b"RESCUET "
    put(boot, 11, "H", sector)
    boot[13] = 1
    put(boot, 14, "H", reserved)
    boot[16] = 2
    boot[21] = 0xF8
    put(boot, 32, "I", total//sector)
    put(boot, 36, "I", fat_sectors)
    put(boot, 44, "I", 2)
    put(boot, 48, "H", 1)
    put(boot, 50, "H", 6)
    boot[66] = 0x29
    boot[82:90] = b"FAT32   "
    boot[510:512] = b"\x55\xaa"
    fat = bytearray(fat_sectors*sector)
    for index, value in {0:0xFFFFFF8,1:0xFFFFFFF,2:8,8:0xFFFFFFF,9:0xFFFFFFF,
                         100:105,105:102,102:0xFFFFFFF,120:121,121:120,
                         500:501,501:0xFFFFFFF}.items():
        put(fat, index*4, "I", value)
    address = lambda c: heap+(c-2)*cluster
    root_clusters = [2,8]
    slots, truth, unsupported, positions = [], {}, [], {}

    def add(name, first, data, supported=True, long_name=None, **kwargs):
        if long_name:
            slots.extend(lfn(long_name, name))
        offset = address(root_clusters[len(slots)//16])+(len(slots)%16)*32
        slots.append(sfn(name, first, len(data), **kwargs))
        key = f"fat32-{offset}"
        positions[name.decode().strip()] = offset
        if supported:
            truth[key] = data
        else:
            unsupported.append(key)
        return key

    add(b"SMALL   TXT", 30, b"known first cluster\n")
    fragmented = b"A"*512+b"B"*512+b"C"*276
    add(b"FRAGMENTBIN", 100, fragmented)
    add(b"LOST    BIN", 110, b"?"*900, supported=False)
    add(b"LOOP    BIN", 120, b"?"*1536, supported=False)
    add(b"REUSED  BIN", 500, b"?"*600, supported=False)
    slots.append(sfn(b"LIVE    BIN", 500, 600, deleted=False))
    add(b"EMPTY   TXT", 0, b"")
    slots.append(sfn(b"FOLDER     ", 9, 0, deleted=False, directory=True))
    if deleted_folder:
        slots.append(sfn(b"DELDIR     ", 10, 0, directory=True))
    # Force a deleted LFN sequence across the fragmented root directory boundary.
    while len(slots) < 14:
        slots.append(sfn(b"FILLER  TXT", 0, 0, deleted=False))
    long_name = "復元できる日本語の長いファイル名.txt"
    assert len(lfn(long_name, b"JAPANE~1TXT")) == 2
    long_key = add(b"JAPANE~1TXT", 130, "日本語ファイル\n".encode(), long_name=long_name)
    child = sfn(b"CHILD   TXT", 140, 14)
    truth[f"fat32-{address(9)}"] = b"nested content"
    if deleted_folder:
        truth[f"fat32-{address(10)}"] = b"child in deleted directory"

    with Path(path).open("xb") as f:
        f.truncate(total)
        def write(offset, data):
            f.seek(offset); f.write(data)
        write(0, boot); write(6*sector, boot)
        for copy in range(2):
            write((reserved+copy*fat_sectors)*sector, fat)
        for index, slot in enumerate(slots):
            write(address(root_clusters[index//16])+(index%16)*32, slot)
        write(address(9), child)
        for cluster_id, data in [(30,b"known first cluster\n"),(100,b"A"*512),(105,b"B"*512),
                                 (102,b"C"*512),(130,"日本語ファイル\n".encode()),(140,b"nested content"),
                                 (500,b"live data"*70)]:
            write(address(cluster_id),data)
        if deleted_folder:
            write(address(10),sfn(b"INNER   TXT",150,len(b"child in deleted directory"),deleted=False))
            write(address(150),b"child in deleted directory")
    return truth, unsupported, {"heap":heap,"cluster":cluster,"fat":reserved*sector,
        "fat_sectors":fat_sectors,"root":2,"long_id":long_key,"long_name":long_name,"positions":positions}


def upcase_codes():
    return [code-32 if 97 <= code <= 122 else code for code in range(65536)]


def exfat_set(name, first, size, initialized=None, contiguous=True, deleted=True, directory=False):
    codes = list(struct.unpack("<"+"H"*(len(name.encode("utf-16le"))//2),name.encode("utf-16le")))
    primary, stream = bytearray(32), bytearray(32)
    primary[0], stream[0] = 0x85, 0xC0
    primary[1] = 1+(len(codes)+14)//15
    put(primary,4,"H",0x10 if directory else 0x20)
    stream[1] = 3 if contiguous else 1
    stream[3] = len(codes)
    upper = [c-32 if 97 <= c <= 122 else c for c in codes]
    put(stream,4,"H",rotate_sum(struct.pack("<"+"H"*len(upper),*upper),16))
    put(stream,8,"Q",size if initialized is None else initialized)
    put(stream,20,"I",first)
    put(stream,24,"Q",size)
    entries = [primary,stream]
    for at in range(0,len(codes),15):
        entry = bytearray(32)
        entry[0] = 0xC1
        for i,code in enumerate(codes[at:at+15]):
            put(entry,2+i*2,"H",code)
        entries.append(entry)
    checksum = rotate_sum(b"".join(entries)[:2]+b"".join(entries)[4:],16)
    put(primary,2,"H",checksum)
    if deleted:
        for entry in entries:
            entry[0] &= 0x7F
    return entries


def exfat(path):
    sector, cluster, count, fat_offset, fat_sectors, heap = 512, 4096, 8192, 24, 65, 128*512
    total = heap+count*cluster
    region = bytearray(12*sector)
    region[:3] = b"\xeb\x76\x90"
    region[3:11] = b"EXFAT   "
    put(region,72,"Q",total//sector)
    put(region,80,"I",fat_offset)
    put(region,84,"I",fat_sectors)
    put(region,88,"I",heap//sector)
    put(region,92,"I",count)
    put(region,96,"I",2)
    put(region,104,"H",0x100)
    region[108:113] = bytes([9,3,1,0x80,0])
    region[510:512] = b"\x55\xaa"
    for index in range(1,9):
        put(region,(index+1)*sector-4,"I",0xAA550000)
    checksum = rotate_sum(bytes(b for i,b in enumerate(region[:11*sector]) if i not in (106,107,112)),32)
    region[11*sector:] = struct.pack("<I",checksum)*(sector//4)
    fat = bytearray(fat_sectors*sector)
    for index,value in {0:0xFFFFFFF8,1:0xFFFFFFFF,2:40,40:0xFFFFFFFF,3:0xFFFFFFFF,
                        110:115,115:112,112:0xFFFFFFFF,120:121,121:120}.items():
        put(fat,index*4,"I",value)
    for index in range(4,36):
        put(fat,index*4,"I",index+1 if index<35 else 0xFFFFFFFF)
    upper = struct.pack("<65536H",*upcase_codes())
    bitmap = bytearray((count+7)//8)
    for cluster_id in [2,3,*range(4,36),40,140,220]:
        bitmap[(cluster_id-2)//8] |= 1 << ((cluster_id-2)%8)
    bitmap_entry, table_entry = bytearray(32), bytearray(32)
    bitmap_entry[0], table_entry[0] = 0x81, 0x82
    put(bitmap_entry,20,"I",3);put(bitmap_entry,24,"Q",len(bitmap))
    put(table_entry,4,"I",rotate_sum(upper,32));put(table_entry,20,"I",4);put(table_entry,24,"Q",len(upper))
    address = lambda c: heap+(c-2)*cluster
    slots, truth, unsupported, positions = [bitmap_entry,table_entry], {}, [], {}
    def add(name, first, data, supported=True, **kwargs):
        offset=address(2 if len(slots)<128 else 40)+(len(slots)%128)*32
        entries=exfat_set(name,first,len(data),**kwargs)
        slots.extend(entries)
        key=f"exfat-{offset}"
        positions[name]=offset
        if supported: truth[key]=data
        else: unsupported.append(key)
        return key
    add("連続配置.bin",100,b"V"*4500+b"\0"*500,initialized=4500)
    add("fragmented.bin",110,b"A"*4096+b"B"*4096+b"C"*808,contiguous=False)
    add("lost.bin",130,b"?"*5000,supported=False,contiguous=False)
    add("loop.bin",120,b"?"*12288,supported=False,contiguous=False)
    add("reused.bin",140,b"?"*1000,supported=False)
    add("empty.txt",0,b"",contiguous=False)
    add("single-cleared.bin",190,b"known first exFAT cluster",contiguous=False)
    slots.extend(exfat_set("DeletedFolder",200,cluster,directory=True))
    slots.extend(exfat_set("LiveFolder",220,cluster,directory=True,deleted=False))
    long_key=add("長"*250+".bin",160,b"long UTF-16 name")
    while len(slots)<127:
        filler=bytearray(32);filler[0]=0xA0;slots.append(filler)
    cross_key=add("cross-cluster.txt",180,b"entry set crosses a root cluster boundary")
    truth[f"exfat-{address(200)}"]=b"child from deleted directory"
    truth[f"exfat-{address(220)}"]=b"child from live directory"
    with Path(path).open("xb") as f:
        f.truncate(total)
        def write(offset,data):
            f.seek(offset);f.write(data)
        write(0,region);write(12*sector,region);write(fat_offset*sector,fat)
        write(address(3),bitmap);write(address(4),upper)
        for i,slot in enumerate(slots):
            write(address(2 if i<128 else 40)+(i%128)*32,slot)
        for cluster_id,data in [(100,b"V"*5000),(110,b"A"*4096),(115,b"B"*4096),(112,b"C"*4096),
                                 (160,b"long UTF-16 name"),(180,b"entry set crosses a root cluster boundary"),
                                 (190,b"known first exFAT cluster"),
                                 (210,b"child from deleted directory"),(230,b"child from live directory")]:
            write(address(cluster_id),data)
        write(address(200),b"".join(exfat_set("inner.txt",210,len(b"child from deleted directory"),deleted=False)))
        write(address(220),b"".join(exfat_set("inner.txt",230,len(b"child from live directory"))))
    return truth,unsupported,{"heap":heap,"cluster":cluster,"fat":fat_offset*sector,
        "root":2,"bitmap":address(3),"long_id":long_key,"cross_id":cross_key,"positions":positions}


def file_hash(path):
    with Path(path).open("rb") as f:
        return hashlib.file_digest(f,"sha256").hexdigest()
