"""FAT32/exFAT CLI + real stdio MCP verification using fresh regular files."""
import argparse
import hashlib
import json
import struct
import tempfile
from pathlib import Path
from e2e import Client, check, run
from make_fat_fixture import fat32, exfat, exfat_set, file_hash, put, rotate_sum, sfn


def scan(binary, source, work, expected=0):
    return run(binary,"scan","--source",source,"--work-dir",work,"--json",expected=expected)


def candidates(binary, session):
    return {c["id"]:c for c in run(binary,"results","--session",session,"--json")["candidates"]}


def write_at(path, offset, data):
    with path.open("r+b") as f:
        f.seek(offset);f.write(data)


def modify_set(path, offset, change):
    with path.open("r+b") as f:
        f.seek(offset);primary=f.read(32)
        f.seek(offset);data=bytearray(f.read((primary[1]+1)*32))
        change(data)
        normalized=bytearray(data)
        for at in range(0,len(data),32):normalized[at]|=0x80
        put(data,2,"H",rotate_sum(normalized[:2]+normalized[4:],16))
        f.seek(offset);f.write(data)


def positive(binary, root, kind, make):
    source=root/(kind+".raw")
    truth,unsupported,metadata=make(source)
    before=file_hash(source)
    summary=scan(binary,source,root/(kind+"-work"))
    check(summary["filesystem"]==kind, "filesystem detection")
    by_id=candidates(binary,summary["session"])
    check(set(by_id)==set(truth)|set(unsupported),"missing/deleted/live candidate mismatch")
    check({key for key,c in by_id.items() if c["supported"]}==set(truth),"unsupported chain accepted")
    if kind=="fat32":
        long=by_id[metadata["long_id"]]
        check(long["name"]==metadata["long_name"] and not long["path_verified"],"deleted LFN reconstruction/provenance")
        check(any(c["original_path"].startswith("FOLDER/") for c in by_id.values()),"nested FAT32 path")
    else:
        check(by_id[metadata["cross_id"]]["name"]=="cross-cluster.txt","entry set across fragmented directory")
        check(any(c["original_path"]=="DeletedFolder/inner.txt" for c in by_id.values()),"deleted exFAT directory")
    output=root/(kind+"-out")
    recovered=run(binary,"recover","--session",summary["session"],"--all","--destination",output,"--json")
    check(len(recovered["results"])==len(truth),"recover all selected unsupported entry")
    for result in recovered["results"]:
        path=Path(result["path"])
        check(result["status"]=="completed" and path.parent.samefile(output),result)
        check(path.read_bytes()==truth[result["candidate_id"]],result)
        check(result["sha256"]==hashlib.sha256(truth[result["candidate_id"]]).hexdigest(),result)
        check(result["extent_evidence"]!="unknown" and result["allocation_status"]!="conflict",result)
    check(file_hash(source)==before,"source image modified")
    prior={p.name:file_hash(p) for p in output.iterdir()}
    key=next(iter(truth))
    run(binary,"recover","--session",summary["session"],"--id",key,"--destination",output,"--json")
    check(all(file_hash(output/name)==digest for name,digest in prior.items()),"existing output overwritten")
    for key in unsupported:
        result=run(binary,"recover","--session",summary["session"],"--id",key,"--destination",root/(kind+"-denied"),"--json",expected=5)["results"][0]
        check(result["path"] is None and result["status"]=="failed",result)

    edited=json.loads(Path(summary["session"]).read_text(encoding="utf-8"))
    edited["candidates"][0]["record"]+=32
    edited_path=root/(kind+"-tampered.json")
    edited_path.write_text(json.dumps(edited),encoding="utf-8")
    result=run(binary,"recover","--session",edited_path,"--id",edited["candidates"][0]["id"],"--destination",root/(kind+"-tampered-out"),"--json",expected=5)["results"][0]
    check(result["path"] is None,"serialized byte offset trusted")

    client=Client(binary,source,root/(kind+"-mcp-work"),root/(kind+"-mcp-out"))
    try:
        client.rpc("initialize",{"protocolVersion":"2025-11-25"})
        job=client.wait(client.tool("start_scan",{"source":str(source),"request_id":"scan-fat"})["job_id"])
        check(job["state"]=="completed",job)
        sid=job["result"]["session_id"]
        listed=client.tool("list_candidates",{"session_id":sid,"limit":100})
        check({c["id"] for c in listed["candidates"]}==set(by_id),"MCP enumeration differs")
        job=client.wait(client.tool("recover_files",{"session_id":sid,"ids":list(truth),
            "destination":str(root/(kind+"-mcp-out")),"request_id":"recover-fat"})["job_id"])
        check(job["state"]=="completed",job)
        for result in job["result"]["results"]:
            check(Path(result["path"]).read_bytes()==truth[result["candidate_id"]],"MCP byte mismatch")
    finally: client.close()
    return len(truth)


def main(binary):
    cases=0
    with tempfile.TemporaryDirectory(prefix="rescue-fat-e2e-") as temporary:
        root=Path(temporary)
        recovered=positive(binary,root,"fat32",fat32)+positive(binary,root,"exfat",exfat)
        cases+=2

        source=root/"fat-live-directory-overlap.raw"
        _,_,meta=fat32(source)
        write_at(source,meta["heap"]+7*32+26,struct.pack("<H",500))
        fake_offset=meta["heap"]+(500-2)*512
        write_at(source,fake_offset,sfn(b"FAKE    TXT",400,12))
        summary=scan(binary,source,root/"fat-live-directory-overlap-work",expected=5)
        check(f"fat32-{fake_offset}" not in candidates(binary,summary["session"]),"live file bytes interpreted as a directory")
        check(any("overlaps" in w for w in summary["warnings"]),"live directory/file crosslink not reported")
        cases+=1
        source=root/"exfat-live-directory-overlap.raw"
        _,_,meta=exfat(source)
        free_slot=meta["heap"]+(40-2)*meta["cluster"]+3*32
        write_at(source,free_slot,b"".join(exfat_set("LiveOwner.bin",220,4096,deleted=False)))
        write_at(source,free_slot+3*32,b"".join(exfat_set("ReusedOwner.bin",140,1000,deleted=False)))
        with source.open("r+b") as f:
            offset=meta["bitmap"]+(140-2)//8
            f.seek(offset);value=f.read(1)[0]
            f.seek(offset);f.write(bytes([value & ~(1<<((140-2)%8))]))
        summary=scan(binary,source,root/"exfat-live-directory-overlap-work",expected=5)
        items=candidates(binary,summary["session"])
        check(not any(c["original_path"]=="LiveFolder/inner.txt" for c in items.values()),"exFAT live file treated as directory")
        check(not items[f"exfat-{meta['positions']['reused.bin']}"]["supported"],"bitmap-only check missed a live allocation reference")
        cases+=1

        source=root/"deleted-directory.raw"
        truth,_,meta=fat32(source,deleted_folder=True)
        summary=scan(binary,source,root/"deleted-directory-work",expected=5)
        check(summary["truncated"],"cleared directory chain was reported as complete")
        by_id=candidates(binary,summary["session"])
        inner=next(c for c in by_id.values() if c["name"]=="INNER.TXT")
        check(not inner["path_verified"],"deleted directory name treated as verified")
        result=run(binary,"recover","--session",summary["session"],"--id",inner["id"],"--destination",root/"deleted-directory-out","--json")["results"][0]
        check(Path(result["path"]).read_bytes()==truth[inner["id"]],"known first directory cluster not recoverable")
        cases+=1

        source=root/"mirror.raw"
        _,_,meta=fat32(source)
        write_at(source,meta["fat"]+meta["fat_sectors"]*512+105*4,struct.pack("<I",104))
        summary=scan(binary,source,root/"mirror-work")
        by_id=candidates(binary,summary["session"])
        fragment=by_id[f"fat32-{meta['positions']['FRAGMENTBIN']}"]
        check(not fragment["supported"] and any("copies disagree" in w for w in fragment["warnings"]),"FAT copy disagreement hidden")
        cases+=1
        source=root/"active-fat.raw"
        _,_,meta=fat32(source)
        for offset in (40,6*512+40):
            write_at(source,offset,struct.pack("<H",0x81))
        write_at(source,meta["fat"]+105*4,struct.pack("<I",104))
        summary=scan(binary,source,root/"active-fat-work")
        fragment=candidates(binary,summary["session"])[f"fat32-{meta['positions']['FRAGMENTBIN']}"]
        check(fragment["supported"] and any("active FAT 1" in w for w in summary["warnings"]),"inactive FAT chosen or selection not reported")
        cases+=1
        source=root/"bad-backup.raw"
        fat32(source)
        write_at(source,6*512+44,struct.pack("<I",9))
        scan(binary,source,root/"bad-backup-work",expected=4)
        cases+=1
        source=root/"bad-lfn.raw"
        _,_,meta=fat32(source)
        write_at(source,meta["heap"]+14*32+13,b"\x00")
        summary=scan(binary,source,root/"bad-lfn-work")
        item=candidates(binary,summary["session"])[meta["long_id"]]
        check(item["supported"] and item["name"]!=meta["long_name"] and any("Invalid/orphaned" in w for w in item["warnings"]),"bad LFN checksum trusted")
        cases+=1

        source=root/"bad-set.raw"
        _,_,meta=exfat(source)
        offset=meta["positions"]["連続配置.bin"]
        write_at(source,offset+8,b"\x55")
        summary=scan(binary,source,root/"bad-set-work",expected=5)
        check(summary["invalid_records"]==1 and f"exfat-{offset}" not in candidates(binary,summary["session"]),"bad exFAT set checksum trusted")
        cases+=1
        source=root/"bad-name-hash.raw"
        _,_,meta=exfat(source)
        modify_set(source,meta["positions"]["連続配置.bin"],lambda data: put(data,32+4,"H",0))
        summary=scan(binary,source,root/"bad-name-hash-work",expected=5)
        check(any("name hash" in w for w in summary["warnings"]),"name hash was not verified with upcase table")
        cases+=1
        for kind,offset_fn in [("boot",lambda m:120),("upcase",lambda m:m["heap"]+2*m["cluster"]+100),
                               ("bitmap",lambda m:m["bitmap"])]:
            source=root/("bad-"+kind+".raw")
            _,_,meta=exfat(source)
            write_at(source,offset_fn(meta),b"\x00" if kind=="bitmap" else b"\x55")
            scan(binary,source,root/("bad-"+kind+"-work"),expected=4)
            cases+=1
        source=root/"huge-data-length.raw"
        _,_,meta=exfat(source)
        offset=meta["positions"]["連続配置.bin"]
        modify_set(source,offset,lambda data: put(data,32+24,"Q",2**64-1))
        summary=scan(binary,source,root/"huge-data-work")
        check(not candidates(binary,summary["session"])[f"exfat-{offset}"]["supported"],"oversized data length accepted")
        cases+=1
        source=root/"cancel.raw"
        _,_,meta=fat32(source)
        cluster_count=(100000*32+511)//512
        chain=[2]+list(range(2000,2000+cluster_count-1))
        with source.open("r+b") as f:
            for copy in range(2):
                for i,cluster in enumerate(chain):
                    f.seek(meta["fat"]+copy*meta["fat_sectors"]*512+cluster*4)
                    f.write(struct.pack("<I",chain[i+1] if i+1<len(chain) else 0xFFFFFFF))
            block=sfn(b"FILLER  TXT",0,0,deleted=False)*16
            for cluster in chain:
                f.seek(meta["heap"]+(cluster-2)*512);f.write(block)
            f.seek(meta["heap"]);f.write(sfn(b"SMALL   TXT",30,len(b"known first cluster\n")))
        client=Client(binary,source,root/"cancel-work",root/"cancel-out")
        try:
            client.rpc("initialize",{"protocolVersion":"2025-11-25"})
            started=client.tool("start_scan",{"source":str(source),"request_id":"cancel-fat"})
            client.tool("cancel_job",{"job_id":started["job_id"]})
            job=client.wait(started["job_id"])
            check(job["state"]=="cancelled",job)
            check(client.tool("get_report",{"session_id":job["result"]["session_id"]})["cancelled"],"no partial cancellation session")
            completed=client.wait(client.tool("start_scan",{"source":str(source),"request_id":"scan-for-recovery"})["job_id"])
            check(completed["state"]=="completed",completed)
            started=client.tool("recover_files",{"session_id":completed["result"]["session_id"],"ids":[f"fat32-{meta['heap']}"],
                "destination":str(root/"cancel-out"),"request_id":"cancel-recovery"})
            client.tool("cancel_job",{"job_id":started["job_id"]})
            job=client.wait(started["job_id"])
            check(job["state"]=="cancelled",job)
        finally: client.close()
        cases+=2
    print(json.dumps({"status":"passed","cases":cases,"byte_exact_files":recovered,
        "scope":"FAT32/exFAT CLI/MCP, allocation conflicts, chains, names/checksums, bounds, cancellation"}))


if __name__=="__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--binary",type=Path,required=True)
    main(parser.parse_args().binary.resolve())
