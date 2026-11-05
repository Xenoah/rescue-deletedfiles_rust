"""All specified format families, with exact byte/hash oracles and honest loss cases.

The runner needs only Python stdlib and the checked-in independently generated
corpus. --independent additionally needs ntfs-3g, mtools/dosfstools, exfatprogs.
It creates and modifies fresh regular images only, never block devices.
"""
import argparse
from collections import defaultdict
import ctypes
from datetime import datetime, timezone, timedelta
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import uuid
import zlib

from e2e import Client, check, run
from make_fixture import record, filename, resident, nonresident, put, MFT_LCN, SECTOR, RECORD
from make_fat_fixture import fat32, exfat, exfat_set, sfn, lfn, file_hash
from e2e_streams import ads_id
from independent_ntfs import mark_deleted


def corpus_load(root):
    manifest=json.loads((root/'manifest.json').read_text(encoding='utf-8'))
    files=manifest['files']
    for entry in files:
        entry=dict(entry)
        entry['path']=(root/entry['file']).resolve()
        check(entry['path'].is_relative_to(root.resolve()),'corpus path escaped')
        entry['data']=entry['path'].read_bytes()
        check(len(entry['data'])==entry['bytes'] and hashlib.sha256(entry['data']).hexdigest()==entry['sha256'],'corpus bytes changed: '+entry['file'])
        yield entry


def lznt1(data):
    """Small fixture encoder; native Windows decoder independently checks its output."""
    result=bytearray()
    for start in range(0,len(data),4096):
        chunk=data[start:start+4096];body=bytearray();at=0;previous=defaultdict(list)
        while at<len(chunk):
            flags_at=len(body);body.append(0)
            for bit in range(8):
                if at>=len(chunk):break
                mask=0xfff;shift=12;position=max(0,at-1)
                while position>=16:mask>>=1;shift-=1;position>>=1
                best_length,best_distance=0,0
                maximum=min(mask+3,len(chunk)-at)
                if maximum>=3:
                    for before in reversed(previous[chunk[at:at+3]][-48:]):
                        distance=at-before
                        if distance>((0xffff>>shift)+1):continue
                        length=3
                        while length<maximum and chunk[before+length]==chunk[at+length]:length+=1
                        if length>best_length:best_length,best_distance=length,distance
                        if length==maximum:break
                count=best_length if best_length>=3 else 1
                if best_length>=3:
                    body[flags_at]|=1<<bit
                    body.extend(struct.pack('<H',((best_distance-1)<<shift)|(best_length-3)))
                else:body.append(chunk[at])
                for index in range(at,at+count):
                    if index+3<=len(chunk):previous[chunk[index:index+3]].append(index)
                at+=count
        if len(body)<len(chunk):result.extend(struct.pack('<H',0xb000|(len(body)-1))+body)
        else:result.extend(struct.pack('<H',0x3000|(len(chunk)-1))+chunk)
    return bytes(result)


def native_lznt1(encoded,expected):
    if sys.platform!='win32':return False
    ntdll=ctypes.WinDLL('ntdll');ulong=ctypes.c_uint32
    ntdll.RtlDecompressBuffer.argtypes=[ctypes.c_ushort,ctypes.c_void_p,ulong,ctypes.c_void_p,ulong,ctypes.POINTER(ulong)]
    ntdll.RtlDecompressBuffer.restype=ctypes.c_int32
    target=ctypes.create_string_buffer(len(expected));source=ctypes.create_string_buffer(encoded);written=ulong()
    status=ntdll.RtlDecompressBuffer(2,target,len(target),source,len(encoded),ctypes.byref(written))
    check(status==0 and target.raw[:written.value]==expected,f'fixture LZNT1 failed native oracle: {status:#x}')
    return True


def ntfs_image(path,files,layout):
    size=8*1024*1024;volume=bytearray(size);boot=bytearray(512)
    boot[:3]=b'\xeb\x52\x90';boot[3:11]=b'NTFS    ';put(boot,11,'H',512);boot[13]=1
    put(boot,40,'Q',size//512-1);put(boot,48,'Q',4);boot[64]=246;boot[510:]=b'\x55\xaa'
    volume[:512]=boot;volume[-512:]=boot
    records=128;cursor=4096;truth={};extents={};native=0
    def save_record(n,attrs,flags=0,sequence=7):
        at=MFT_LCN*SECTOR+n*RECORD;volume[at:at+RECORD]=record(n,attrs,flags,sequence)
    save_record(0,[nonresident([(records*2,MFT_LCN)],records*RECORD)],1,1)
    save_record(5,[filename('.')],3,1);save_record(10,[filename('全形式',parent=5)],3,1)
    for index,entry in enumerate(files,start=20):
        data=entry['data'];runs=[];ranges=[]
        if layout=='compressed':
            for at in range(0,len(data),8192):
                unit=data[at:at+8192]
                if not any(unit):runs.append((16,None));continue
                encoded=lznt1(unit);count=math.ceil(len(encoded)/512)
                if count<16:
                    native+=int(native_lznt1(encoded,unit));stored=encoded
                    runs.extend([(count,cursor),(16-count,None)])
                else:stored=unit;count=16;runs.append((16,cursor))
                volume[cursor*512:cursor*512+len(stored)]=stored;ranges.append((cursor*512,count*512));cursor+=count+2
        elif layout=='fragmented':
            for at in range(0,len(data),512):
                block=data[at:at+512];runs.append((1,cursor))
                volume[cursor*512:cursor*512+len(block)]=block;ranges.append((cursor*512,512));cursor+=2
        else:
            count=math.ceil(len(data)/512);runs=[(count,cursor)]
            volume[cursor*512:cursor*512+len(data)]=data;ranges.append((cursor*512,count*512));cursor+=count+2
        stream='corpus' if layout=='ads' else None
        attribute=nonresident(runs,len(data),flags=1 if layout=='compressed' else 0,name=stream,compression=4 if layout=='compressed' else 0)
        attrs=[filename(entry['path'].name,parent=10),attribute]
        if stream:attrs.insert(1,resident(0x80,b'ADS carrier'))
        save_record(index,attrs)
        key=ads_id(index,stream) if stream else f'mft-{index}-7'
        truth[key]=entry;extents[key]=ranges
    check(cursor*512<size-512,'fixture allocation exceeds volume')
    with path.open('xb') as f:f.write(volume)
    return truth,{'extents':extents,'native_lznt1_units':native}


def fat_image(path,files,kind,fragmented):
    _,_,meta=(fat32 if kind=='fat32' else exfat)(path)
    address=lambda c:meta['heap']+(c-2)*meta['cluster']
    cursor=1000;truth={};slots=[]
    if kind=='fat32':
        fat=bytearray(meta['fat_sectors']*512);struct.pack_into('<II',fat,0,0xffffff8,0xfffffff)
        root_clusters=list(range(2,18))
        for i,c in enumerate(root_clusters):put(fat,c*4,'I',root_clusters[i+1] if i+1<len(root_clusters) else 0xfffffff)
    else:
        with path.open('rb') as f:f.seek(address(2));system=f.read(64);f.seek(meta['fat']);fat=bytearray(f.read(65*512))
        root_clusters=[2,40];slots=[bytearray(system[:32]),bytearray(system[32:])]
    with path.open('r+b') as f:
        for index,entry in enumerate(files):
            count=math.ceil(len(entry['data'])/meta['cluster']);clusters=[cursor+i*(2 if fragmented else 1) for i in range(count)]
            cursor=clusters[-1]+3
            if kind=='fat32' or fragmented:
                for i,c in enumerate(clusters):put(fat,c*4,'I',clusters[i+1] if i+1<len(clusters) else (0xfffffff if kind=='fat32' else 0xffffffff))
            if kind=='fat32':
                short=f'F{index:07d}'.encode()+entry['extension'].upper().ljust(3)[:3].encode()
                slots.extend(lfn(entry['path'].name,short))
                offset=address(root_clusters[len(slots)//16])+(len(slots)%16)*32
                slots.append(sfn(short,clusters[0],len(entry['data'])))
            else:
                offset=address(root_clusters[len(slots)//128])+(len(slots)%128)*32
                slots.extend(exfat_set(entry['path'].name,clusters[0],len(entry['data']),contiguous=not fragmented))
            truth[f'{kind}-{offset}']=entry
            for i,c in enumerate(clusters):
                block=entry['data'][i*meta['cluster']:(i+1)*meta['cluster']]
                f.seek(address(c));f.write(block)
        slots.append(bytes(32))
        per=meta['cluster']//32
        for i,slot in enumerate(slots):f.seek(address(root_clusters[i//per])+(i%per)*32);f.write(slot)
        for copy in range(2 if kind=='fat32' else 1):
            f.seek(meta['fat']+copy*len(fat));f.write(fat)
    return truth,{}


def record_results(case,frontend,files,truth,results,rows,strict=True):
    check(len(results)==len(truth),'selected files missing: '+case)
    for result in results:
        entry=truth[result['candidate_id']];path=result.get('path');completed=result['status']=='completed'
        data=Path(path).read_bytes() if completed else None
        equal=data==entry['data']
        digest=hashlib.sha256(data).hexdigest() if data is not None else None
        if completed:check(result['sha256']==digest and result['bytes_written']==len(data),'output hash/report mismatch')
        if strict:check(completed and equal,f'{case}/{entry["file"]}: byte mismatch {result}')
        rows.append({'case':case,'frontend':frontend,'format':entry['format'],'file':entry['file'],'bytes':entry['bytes'],
                     'status':result['status'],'byte_exact':equal,'original_sha256':entry['sha256'],'output_sha256':digest,
                     'expected': 'byte_exact' if strict else 'not_recoverable', 'error':result.get('error')})


def positive(binary,source,truth,root,case,rows,hash_source=None):
    hash_source=hash_source or source
    before=file_hash(hash_source);work=root/(case+'-cli-work');out=root/(case+'-cli-out')
    scan=run(binary,'scan','--source',source,'--work-dir',work,'--json')
    listed=run(binary,'results','--session',scan['session'],'--limit','1000','--json')['candidates']
    check(set(truth).issubset({c['id'] for c in listed if c['supported']}),'missing candidate: '+case)
    args=[v for key in truth for v in ['--id',key]]
    result=run(binary,'recover','--session',scan['session'],*args,'--destination',out,'--json')
    record_results(case,'cli',None,truth,result['results'],rows)
    client=Client(binary,source,root/(case+'-mcp-work'),root/(case+'-mcp-out'))
    try:
        client.rpc('initialize',{'protocolVersion':'2025-11-25'})
        job=client.wait(client.tool('start_scan',{'source':str(source),'request_id':'scan'})['job_id'])
        check(job['state']=='completed',job);sid=job['result']['session_id']
        listed=client.tool('list_candidates',{'session_id':sid,'limit':1000})['candidates']
        check(set(truth).issubset({c['id'] for c in listed if c['supported']}),'MCP candidates differ')
        job=client.wait(client.tool('recover_files',{'session_id':sid,'ids':list(truth),'destination':str(root/(case+'-mcp-out')),'request_id':'recover'})['job_id'])
        check(job['state']=='completed',job)
        record_results(case,'mcp',None,truth,job['result']['results'],rows)
    finally:client.close()
    check(file_hash(hash_source)==before,'source image modified: '+case)
    print(case,':',len(truth)*2,'byte-exact CLI/MCP files',flush=True)


def carving(binary,source,files,root,case,rows,large_png=False,erase_metadata=True):
    # Remove volume boot and all MFT records, preserving only file content.
    if erase_metadata:
        with source.open('r+b') as f:f.write(bytes(512));f.seek(MFT_LCN*SECTOR);f.write(bytes(128*RECORD))
    if erase_metadata:run(binary,'scan','--source',source,'--work-dir',root/(case+'-quick'),'--json',expected=4)
    before=file_hash(source)
    scan=run(binary,'scan','--mode','carve','--source',source,'--work-dir',root/(case+'-work'),'--json',expected=5)
    listed=run(binary,'results','--session',scan['session'],'--limit','1000','--json')['candidates']
    selected=[c['id'] for c in listed if c['supported']]
    results=run(binary,'recover','--session',scan['session'],*[v for key in selected for v in ['--id',key]],'--destination',root/(case+'-out'),'--json')['results']
    hashes={r['sha256'] for r in results if r['status']=='completed'}
    check(all(r['status']=='completed' and hashlib.sha256(Path(r['path']).read_bytes()).hexdigest()==r['sha256'] for r in results),'carving output hash mismatch')
    for entry in files:
        expected=(entry['extension']=='png' and (large_png or entry['file']!='files/noise.png')) or (entry['extension']=='jpg' and large_png)
        exact=entry['sha256'] in hashes
        check(exact==expected,'unexpected top-level carving recovery: '+entry['file'])
        rows.append({'case':case,'frontend':'cli','format':entry['format'],'file':entry['file'],'bytes':entry['bytes'],
                     'status':'completed' if exact else 'not_recovered','byte_exact':exact,'original_sha256':entry['sha256'],
                     'expected':'byte_exact' if expected else 'unsupported','output_sha256':entry['sha256'] if exact else None})
    check(file_hash(source)==before,'carving source modified')
    print(case,':',sum(entry['sha256'] in hashes for entry in files),'of',len(files),'original files exact; other top-level formats unsupported',flush=True)


def partition_image(path,volume,gpt):
    data=volume.read_bytes();sectors=len(data)//512;total=2048+sectors+2048
    disk=bytearray(total*512);disk[2048*512:(2048+sectors)*512]=data
    disk[510:512]=b'\x55\xaa'
    disk[446:462]=struct.pack('<B3sB3sII',0,b'\x00\x20\x21',0xee if gpt else 7,b'\xfe\xff\xff',1 if gpt else 2048,total-1 if gpt else sectors)
    if gpt:
        entries=bytearray(128*128)
        entries[:16]=uuid.UUID('EBD0A0A2-B9E5-4433-87C0-68B6B72699C7').bytes_le
        entries[16:32]=uuid.UUID('d64c9f00-0000-4000-8000-000000000001').bytes_le
        struct.pack_into('<QQQ',entries,32,2048,2048+sectors-1,0)
        name='Rescue Corpus'.encode('utf-16le');entries[56:56+len(name)]=name
        checksum=zlib.crc32(entries)
        def header(current,backup,table):
            b=bytearray(512);b[:8]=b'EFI PART';struct.pack_into('<II',b,8,0x10000,92)
            struct.pack_into('<QQQQ',b,24,current,backup,34,total-34)
            b[56:72]=uuid.UUID('d64c9f00-0000-4000-8000-000000000002').bytes_le
            struct.pack_into('<QIII',b,72,table,128,128,checksum)
            struct.pack_into('<I',b,16,zlib.crc32(b[:92]));return b
        disk[512:1024]=header(1,total-1,2)
        disk[1024:1024+len(entries)]=entries
        disk[(total-33)*512:(total-1)*512]=entries
        disk[-512:]=header(total-1,1,total-33)
    with path.open('xb') as f:f.write(disk)


def independent(binary,files,root,rows):
    tools=['mkntfs','ntfscp','ntfsinfo','mkfs.fat','mcopy','mdel','mkfs.exfat']
    check(all(shutil.which(tool) for tool in tools),'independent filesystem tools are required')
    def command(*args):return subprocess.check_output(list(map(str,args)),stderr=subprocess.STDOUT,text=True,encoding='utf-8')
    def new(path,size):
        with path.open('xb') as f:f.truncate(size)
    source=root/'independent-ntfs.raw';new(source,64*1024*1024)
    command('mkntfs','-F','-Q','-s','512','-c','4096',source)
    records=[]
    for entry in files:
        command('ntfscp',source,entry['path'],'/'+entry['path'].name)
        info=command('ntfsinfo','-F','/'+entry['path'].name,source)
        records.append(int(re.search(r'Dumping Inode (\d+)',info).group(1)))
    truth={f'mft-{number}-{mark_deleted(source,number)}':entry for number,entry in zip(records,files)}
    positive(binary,source,truth,root,'independent-ntfs-emulated-delete',rows)
    for spc,size in [(1,64*1024*1024),(8,512*1024*1024)]:
        case=f'fat32-mtools-delete-cluster-{spc*512}';source=root/(case+'.raw');new(source,size)
        command('mkfs.fat','-F','32','-s',str(spc),source)
        names=[]
        for index,entry in enumerate(files):
            name=f'fixture-{index:03d}.'+entry['extension'];names.append(name)
            command('mcopy','-i',source,entry['path'],'::'+name)
        for name in names:command('mdel','-i',source,'::'+name)
        before=file_hash(source)
        summary=run(binary,'scan','--source',source,'--work-dir',root/(case+'-work'),'--json')
        candidates=run(binary,'results','--session',summary['session'],'--limit','1000','--json')['candidates']
        by_name={c['name']:c for c in candidates};check(set(by_name)==set(names),'actual FAT deletion lost names')
        for name,entry in zip(names,files):
            c=by_name[name];expected=entry['bytes']<=spc*512
            check(c['supported']==expected,'cleared chain support mismatch: '+name)
            result=run(binary,'recover','--session',summary['session'],'--id',c['id'],'--destination',root/(case+'-out'),'--json',expected=0 if expected else 5)['results']
            record_results(case,'cli',None,{c['id']:entry},result,rows,strict=expected)
        check(file_hash(source)==before,'actual-deletion image changed')
        print(case,':',sum(e['bytes']<=spc*512 for e in files),'of',len(files),'exact; cleared multi-cluster chains are unsupported',flush=True)
    source=root/'independent-exfat.raw';new(source,64*1024*1024);command('mkfs.exfat','-L','RESCUE',source)
    truth={}
    with source.open('r+b') as f:
        boot=f.read(512);sector=1<<boot[108];cluster=sector<<boot[109];heap=struct.unpack_from('<I',boot,88)[0]*sector
        root_cluster=struct.unpack_from('<I',boot,96)[0];address=lambda c:heap+(c-2)*cluster
        f.seek(address(root_cluster));directory=f.read(cluster)
        free=next(at for at in range(0,len(directory),32) if directory[at]==0)
        bm=next(directory[at:at+32] for at in range(0,len(directory),32) if directory[at]==0x81)
        f.seek(address(struct.unpack_from('<I',bm,20)[0]));bitmap=f.read(struct.unpack_from('<Q',bm,24)[0])
        cursor=max(root_cluster+128,2048)
        for entry in files:
            count=math.ceil(entry['bytes']/cluster)
            check(all(not bitmap[(c-2)//8]&(1<<((c-2)%8)) for c in range(cursor,cursor+count)),'not a free test extent')
            slots=b''.join(exfat_set(entry['path'].name,cursor,entry['bytes']))
            check(free+len(slots)+32<=cluster,'root directory fixture is full')
            offset=address(root_cluster)+free;f.seek(offset);f.write(slots);free+=len(slots)
            f.seek(address(cursor));f.write(entry['data']);cursor+=count+2
            truth[f'exfat-{offset}']=entry
        f.seek(address(root_cluster)+free);f.write(bytes(32))
    positive(binary,source,truth,root,'independent-exfat-emulated-delete',rows)


def overwrite(binary,source,truth,extents,root,rows):
    for ranges in extents.values():
        with source.open('r+b') as f:
            for at,length in ranges:f.seek(at);f.write(bytes(length))
    summary=run(binary,'scan','--source',source,'--work-dir',root/'overwritten-work','--json')
    args=[v for key in truth for v in ['--id',key]]
    results=run(binary,'recover','--session',summary['session'],*args,'--destination',root/'overwritten-out','--json')['results']
    record_results('overwritten-before-scan','cli',None,truth,results,rows,strict=False)
    check(not any(row['byte_exact'] for row in rows if row['case']=='overwritten-before-scan'),'zeroed originals recovered unexpectedly')
    print('overwritten-before-scan : 0 original files exact (expected data loss)',flush=True)


def main(binary,corpus,output,use_independent):
    output.mkdir(parents=True,exist_ok=True);root=output/'work';root.mkdir(exist_ok=False)
    files=list(corpus_load(corpus));rows=[];sources={};native=0;started=time.monotonic()
    for layout in ['contiguous','fragmented','compressed','ads']:
        case='ntfs-'+layout;source=root/(case+'.raw');truth,meta=ntfs_image(source,files,layout)
        native+=meta['native_lznt1_units'];positive(binary,source,truth,root,case,rows);sources[case]=(source,truth,meta)
    for kind in ['fat32','exfat']:
        for fragmented in [False,True]:
            case=kind+('-fragmented' if fragmented else '-contiguous');source=root/(case+'.raw')
            truth,meta=fat_image(source,files,kind,fragmented);positive(binary,source,truth,root,case,rows)
    for name,expected_large in [('ntfs-contiguous',True),('ntfs-fragmented',False)]:
        copy=root/(name+'-metadata-lost.raw');shutil.copyfile(sources[name][0],copy)
        carving(binary,copy,files,root,name+'-metadata-lost',rows,expected_large)
    for scheme in ['mbr','gpt']:
        disk=root/('whole-disk-'+scheme+'.raw');partition_image(disk,sources['ntfs-contiguous'][0],scheme=='gpt')
        positive(binary,disk,{'p1:'+key:value for key,value in sources['ntfs-contiguous'][1].items()},root,'whole-disk-'+scheme+'-metadata',rows)
        carving(binary,disk,files,root,'whole-disk-'+scheme,rows,True,False)
    unavailable=[]
    for feature,args in [('RAID 0/1/5/6/10/JBOD',['raid','--help']),('raw-device imaging',['image','--help']),('checkpoint resume',['resume','--help'])]:
        call=subprocess.run([str(binary),*args],capture_output=True,text=True,encoding='utf-8',timeout=15)
        check(call.returncode==2,'an unimplemented command unexpectedly exists')
        unavailable.append({'feature':feature,'exit_code':call.returncode,'status':'not_implemented'})
    copied=root/'overwritten.raw';shutil.copyfile(sources['ntfs-contiguous'][0],copied)
    overwrite(binary,copied,sources['ntfs-contiguous'][1],sources['ntfs-contiguous'][2]['extents'],root,rows)
    if use_independent:independent(binary,files,root,rows)
    result={'schema_version':1,'time_jst':datetime.now(timezone(timedelta(hours=9))).isoformat(),
            'platform':platform.platform(),'python':platform.python_version(),'binary':str(binary),
            'binary_sha256':file_hash(binary),'formats':len({e['format'] for e in files}),'corpus_files':len(files),
            'native_lznt1_oracle_units':native,'independent_filesystem_tools':use_independent,
            'elapsed_seconds':round(time.monotonic()-started,3),
            'scope':{'synthetic_regular_images':True,'real_device_tested':False,'windows_gui_tested':False,
                     'real_trim_tested':False,'paid_product_comparison':False},
            'unimplemented_capabilities':unavailable,'rows':rows}
    (output/'results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'status':'passed','formats':result['formats'],'corpus_files':len(files),'observations':len(rows),
                      'byte_exact':sum(row['byte_exact'] for row in rows),'native_lznt1_units':native,'seconds':result['elapsed_seconds']}),flush=True)
    return result


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--corpus',type=Path,default=Path(__file__).resolve().parent.parent/'tests/corpus')
    parser.add_argument('--output',type=Path,required=True);parser.add_argument('--independent',action='store_true')
    args=parser.parse_args();main(args.binary.resolve(),args.corpus.resolve(),args.output.resolve(),args.independent)
