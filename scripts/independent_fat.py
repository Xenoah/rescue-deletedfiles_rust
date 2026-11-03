"""Independent-tool FAT checks using disposable regular images only.

FAT32 uses mkfs.fat + mcopy + mdel (actual directory/FAT deletion by mtools).
exFAT uses mkfs.exfat system metadata and an explicitly emulated deleted entry.
"""
import argparse
import json
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path
from e2e import check, run
from make_fat_fixture import exfat_set, file_hash


def command(*args):
    return subprocess.check_output(list(map(str,args)),stderr=subprocess.STDOUT,text=True,encoding="utf-8")


def new_image(path):
    with path.open("xb") as f:
        f.truncate(64*1024*1024)


def recover_all(binary,image,work,out):
    before=file_hash(image)
    summary=run(binary,"scan","--source",image,"--work-dir",work,"--json")
    result=run(binary,"recover","--session",summary["session"],"--all","--destination",out,"--json")
    check(file_hash(image)==before,"source was modified")
    return summary,result


def main(binary):
    for tool in ("mkfs.fat","mcopy","mdel","mkfs.exfat"):
        if shutil.which(tool) is None:
            raise SystemExit(f"Required independent tool missing: {tool}")
    with tempfile.TemporaryDirectory(prefix="rescue-independent-fat-") as temporary:
        root=Path(temporary)
        image=root/"fat32.raw"
        new_image(image)
        command("mkfs.fat","-F","32","-s","1",image)
        proof=root/"proof.txt"
        data=b"Independent FAT32 deletion by mtools\n"*3
        proof.write_bytes(data)
        for name in ("PROOF.TXT","Long recovery name.txt","復元用の長い名前.txt"):
            command("mcopy","-i",image,proof,"::"+name)
        for name in ("PROOF.TXT","Long recovery name.txt","復元用の長い名前.txt"):
            command("mdel","-i",image,"::"+name)
        summary,result=recover_all(binary,image,root/"fat-work",root/"fat-out")
        check(summary["candidates"]==3 and len(result["results"])==3,summary)
        for item in result["results"]:
            check(item["status"]=="completed" and Path(item["path"]).read_bytes()==data,item)
        names=run(binary,"results","--session",summary["session"],"--json")["candidates"]
        check(any(c["name"]=="復元用の長い名前.txt" for c in names),"independent UTF-16 LFN")

        image=root/"exfat.raw"
        new_image(image)
        command("mkfs.exfat","-L","RESCUE",image)
        with image.open("r+b") as f:
            boot=f.read(512)
            sector=1<<boot[108]
            cluster=sector<<boot[109]
            heap=struct.unpack_from("<I",boot,88)[0]*sector
            root_cluster=struct.unpack_from("<I",boot,96)[0]
            root_offset=heap+(root_cluster-2)*cluster
            f.seek(root_offset);directory=f.read(cluster)
            free=next(i for i in range(0,len(directory),32) if directory[i]==0)
            bitmap_slot=next(directory[i:i+32] for i in range(0,len(directory),32) if directory[i]==0x81)
            bitmap_cluster=struct.unpack_from("<I",bitmap_slot,20)[0]
            bitmap_size=struct.unpack_from("<Q",bitmap_slot,24)[0]
            # mkfs.exfat's small-volume bitmap is a single contiguous allocation.
            f.seek(heap+(bitmap_cluster-2)*cluster);bitmap=f.read(bitmap_size)
            first=root_cluster+128
            original=bytes(range(251))*40
            initialized=len(original)-500
            needed=(len(original)+cluster-1)//cluster
            for c in range(first,first+needed):
                check(not (bitmap[(c-2)//8] & (1<<((c-2)%8))),"test allocation is not free")
            entries=b"".join(exfat_set("Independent recovery.bin",first,len(original),initialized=initialized))
            check(free+len(entries)+32<=cluster,"test root directory has no room")
            f.seek(root_offset+free);f.write(entries)
            f.seek(heap+(first-2)*cluster);f.write(original)
        summary,result=recover_all(binary,image,root/"exfat-work",root/"exfat-out")
        check(summary["candidates"]==1 and len(result["results"])==1,summary)
        item=result["results"][0]
        expected=original[:initialized]+b"\0"*(len(original)-initialized)
        check(item["status"]=="completed" and Path(item["path"]).read_bytes()==expected,item)
        print(json.dumps({"status":"passed","fat32_files":3,"exfat_bytes":len(expected),
            "scope":"mkfs.fat+mcopy+mdel; mkfs.exfat boot/FAT/bitmap/upcase with emulated deleted entry"}))


if __name__=="__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--binary",type=Path,required=True)
    main(parser.parse_args().binary.resolve())
