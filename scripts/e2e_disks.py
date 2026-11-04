"""Whole-disk recovery, partition corruption and optional read-only native virtual disks.

Never opens a host disk for writing. Native devices are attached read-only from newly
created fixture files. Only the fixture paths are passed to losetup/hdiutil/DiskImage.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
import uuid
import zlib
from e2e_formats import corpus_load, ntfs_image, fat_image, positive, file_hash, run, check

ROOT = Path(__file__).resolve().parent.parent


def disk_image(path, volumes, sector=512, scheme='gpt'):
    starts=[]; at=2048
    for volume in volumes:
        count=(volume.stat().st_size+sector-1)//sector; starts.append((at,count)); at+=count+2048
    total=at+2048
    with path.open('xb') as f:
        f.truncate(total*sector)
        for volume,(start,count) in zip(volumes,starts):
            f.seek(start*sector)
            with volume.open('rb') as source:shutil.copyfileobj(source,f)
        boot=bytearray(sector);boot[510:512]=b'\x55\xaa'
        if scheme=='mbr':
            check(len(starts)<=4,'too many MBR partitions')
            for i,(start,count) in enumerate(starts):struct.pack_into('<B3sB3sII',boot,446+16*i,0,b'\0'*3,7,b'\0'*3,start,count)
        else:
            struct.pack_into('<B3sB3sII',boot,446,0,b'\0'*3,0xee,b'\0'*3,1,min(total-1,0xffffffff))
            entries=bytearray(128*128)
            for i,(start,count) in enumerate(starts):
                pos=i*128;entries[pos:pos+16]=uuid.UUID('EBD0A0A2-B9E5-4433-87C0-68B6B72699C7').bytes_le
                entries[pos+16:pos+32]=uuid.uuid4().bytes_le
                struct.pack_into('<QQQ',entries,pos+32,start,start+count-1,0)
                name=f'Rescue test {i+1}'.encode('utf-16le');entries[pos+56:pos+56+len(name)]=name
            table_sectors=(len(entries)+sector-1)//sector;guid=uuid.uuid4().bytes_le
            def header(current,backup,table):
                h=bytearray(sector);h[:8]=b'EFI PART';struct.pack_into('<II',h,8,0x10000,92)
                struct.pack_into('<QQQQ',h,24,current,backup,2+table_sectors,total-table_sectors-2)
                h[56:72]=guid;struct.pack_into('<QIII',h,72,table,128,128,zlib.crc32(entries))
                struct.pack_into('<I',h,16,zlib.crc32(h[:92]));return h
            f.seek(sector);f.write(header(1,total-1,2));f.seek(2*sector);f.write(entries)
            f.seek((total-table_sectors-1)*sector);f.write(entries);f.seek((total-1)*sector);f.write(header(total-1,1,total-table_sectors-1))
        f.seek(0);f.write(boot)
    return starts


def call(*args):
    p=subprocess.run([str(a) for a in args],capture_output=True,text=True,encoding='utf-8',timeout=120)
    check(p.returncode==0,f'{args[0]} failed: {p.stdout[-2000:]} {p.stderr[-2000:]}')
    return p.stdout.strip()


def fixed_vhd(raw,path):
    shutil.copyfile(raw,path);size=raw.stat().st_size
    footer=bytearray(512);footer[:8]=b'conectix'
    struct.pack_into('>IIQ',footer,8,2,0x10000,0xffffffffffffffff)
    footer[28:32]=b'RSCU';struct.pack_into('>I',footer,32,0x10000);footer[36:40]=b'Wi2k'
    struct.pack_into('>QQ',footer,40,size,size)
    # VHD CHS geometry follows the Microsoft algorithm; capacity uses CurrentSize.
    sectors=min(size//512,65535*16*255)
    if sectors>=65535*16*63:heads,spt=16,255;tracks=sectors//spt
    else:
        spt=17;tracks=sectors//spt;heads=max(4,(tracks+1023)//1024)
        if tracks>=heads*1024 or heads>16:spt,heads=31,16;tracks=sectors//spt
        if tracks>=heads*1024:spt,heads=63,16;tracks=sectors//spt
    struct.pack_into('>HBBI',footer,56,tracks//heads,heads,spt,2)
    footer[68:84]=uuid.uuid4().bytes;struct.pack_into('>I',footer,64,(~sum(footer))&0xffffffff)
    with path.open('ab') as f:f.write(footer)


@contextlib.contextmanager
def native_source(raw,root):
    system=platform.system()
    if system=='Linux':
        device=call('sudo','losetup','--find','--show','--read-only','--partscan',raw)
        check(device.startswith('/dev/loop') and device[9:].isdigit(),'unexpected loop device')
        try:yield ['sudo'],Path(device),raw
        finally:call('sudo','losetup','--detach',device)
    elif system=='Darwin':
        result=subprocess.run(['/usr/bin/hdiutil','attach','-readonly','-nomount','-imagekey','diskimage-class=CRawDiskImage',str(raw),'-plist'],capture_output=True,check=True)
        import plistlib
        nodes=plistlib.loads(result.stdout)['system-entities']
        device=next(n['dev-entry'] for n in nodes if n.get('dev-entry','').startswith('/dev/disk') and n['dev-entry'][9:].isdigit())
        try:yield ['sudo'],Path('/dev/r'+device[5:]),raw
        finally:call('/usr/bin/hdiutil','detach',device)
    elif system=='Windows':
        vhd=root/'native-fixture.vhd';fixed_vhd(raw,vhd)
        # Environment values keep the fixture path out of PowerShell source code.
        env=os.environ.copy();env['RESCUE_TEST_VHD']=str(vhd)
        def ps(script):
            p=subprocess.run(['powershell.exe','-NoLogo','-NoProfile','-NonInteractive','-Command',"$ErrorActionPreference='Stop';"+script],env=env,capture_output=True,text=True,encoding='utf-8',timeout=120)
            check(p.returncode==0,p.stdout+p.stderr);return p.stdout.strip()
        ps('Mount-DiskImage -ImagePath $env:RESCUE_TEST_VHD -Access ReadOnly -NoDriveLetter | Out-Null')
        try:
            number=int(ps('$i=Get-DiskImage -ImagePath $env:RESCUE_TEST_VHD; if(-not $i.Attached){throw "fixture not attached"}; ($i | Get-Disk).Number'))
            yield [],Path(r'\\.\PhysicalDrive'+str(number)),vhd
        finally:ps('Dismount-DiskImage -ImagePath $env:RESCUE_TEST_VHD | Out-Null')
    else:raise RuntimeError('unsupported native test OS')


def native_test(binary,disk,truth,root,rows):
    before=file_hash(disk)
    with native_source(disk,root) as (prefix,device,backing):
        backing_before=file_hash(backing)
        # A tiny wrapper preserves normal CLI/MCP driver coverage under sudo on Unix.
        executable=binary
        if prefix:
            wrapper=root/'rescue-native'
            import shlex
            wrapper.write_text('#!/bin/sh\nexec '+shlex.join([*prefix,str(binary)])+' "$@"\n');wrapper.chmod(0o755);executable=wrapper
        layout=run(executable,'inspect','--source',device,'--json')['layout']
        check(layout['device'] and layout['scheme']=='gpt' and len(layout['regions'])==3,'native layout mismatch')
        inventory=run(executable,'sources','--json')['sources']
        check(any(str(s['path']).lower()==str(device).lower() for s in inventory),'native disk missing from inventory')
        if platform.system()=='Darwin':
            import plistlib
            fields=['DeviceIdentifier','ParentWholeDisk','PartOfWhole','WholeDisk','Whole','APFSContainerReference']
            diagnostics={}
            output_device=subprocess.check_output(['/bin/df','-P',str(root)],text=True,timeout=30).splitlines()[1].split()[0]
            for label,target in [('source',str(device)),('output',output_device)]:
                value=plistlib.loads(subprocess.check_output(['/usr/sbin/diskutil','info','-plist',target],timeout=30))
                diagnostics[label]={key:value[key] for key in fields if key in value}
            print('macOS disk topology:',json.dumps(diagnostics),flush=True)
        positive(executable,device,truth,root,'native-read-only-disk',rows,hash_source=backing)
        # The output guard can be checked without reading user file contents or writing anything.
        host_same=None
        for item in inventory:
            result=subprocess.run([str(executable),'check-destination','--source',item['path'],'--destination',str(root),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=60)
            if 'the work/recovery destination is on the scanned disk' in result.stdout+result.stderr:
                host_same=item['path'];break
        check(host_same is not None,'same-physical-disk protection not exercised')
        forbidden=root/'must-not-be-created'
        p=subprocess.run([str(executable),'scan','--source',host_same,'--work-dir',str(forbidden),'--json'],capture_output=True,text=True,encoding='utf-8',timeout=60)
        check(p.returncode!=0 and 'UNSAFE_DESTINATION' in p.stdout+p.stderr and not forbidden.exists(),'work directory created on scanned disk')
        # Reuse the synthetic session to exercise each write entry point. The same-
        # disk guard must reject before validating payloads or creating a directory.
        original=next((root/'native-read-only-disk-cli-work').glob('*.rescue-session.json'))
        unsafe=json.loads(original.read_text(encoding='utf-8'));unsafe['source']=host_same
        unsafe_session=root/'same-disk-session.json';unsafe_session.write_text(json.dumps(unsafe),encoding='utf-8')
        for args in [
            ['recover','--session',unsafe_session,'--all','--destination',forbidden,'--stream','--json'],
            ['recover','--session',unsafe_session,'--all','--archive',forbidden/'output.zip','--work-dir',forbidden/'work','--stream','--json'],
        ]:
            p=subprocess.run([str(executable),*map(str,args)],input='',capture_output=True,text=True,encoding='utf-8',timeout=60)
            check(p.returncode!=0 and 'UNSAFE_DESTINATION' in p.stdout+p.stderr and not forbidden.exists(),'unsafe recovery/ZIP/MCP created output')
        p=subprocess.run([str(executable),'mcp','--stdio','--allow-source',str(host_same),'--work-dir',str(forbidden),'--allow-output-root',str(root)],input='',capture_output=True,text=True,encoding='utf-8',timeout=60)
        check(p.returncode==0 and not forbidden.exists(),'same-disk MCP must keep sessions in memory without creating work directories')
        check(file_hash(backing)==backing_before,'read-only backing file changed while attached')
    check(file_hash(backing)==backing_before,'backing file changed on detach')
    check(file_hash(disk)==before,'native source changed')
    return {'device_path':str(device),'backing_sha256_before_and_after':backing_before,'raw_image_sha256_before_and_after':before,'legacy_same_disk_entry_points_rejected':['scan work','stream folder recovery','stream ZIP work/output'],'same_disk_mcp_memory_only':True}


def main(binary,output,native):
    output.mkdir(parents=True,exist_ok=False);files=list(corpus_load(ROOT/'tests/corpus'));rows=[];volumes=[];truth={}
    for i,kind in enumerate(['ntfs','fat32','exfat'],1):
        volume=output/(kind+'.raw')
        original,_=ntfs_image(volume,files,'fragmented') if kind=='ntfs' else fat_image(volume,files,kind,True)
        volumes.append(volume);truth.update({'p'+str(i)+':'+k:v for k,v in original.items()})
    for scheme,sector in [('mbr',512),('gpt',512),('gpt',4096)]:
        case=f'{scheme}-{sector}';disk=output/(case+'.raw');disk_image(disk,volumes,sector,scheme)
        layout=run(binary,'inspect','--source',disk,'--json')['layout']
        check(len(layout['regions'])==3 and layout['sector_size']==sector,'layout mismatch')
        positive(binary,disk,truth,output,case,rows)
    disk=output/'gpt-512.raw'
    for name,offsets in [('protective-mbr',[510]),('primary-header',[512+16]),('primary-table',[1024]),('backup-header',[disk.stat().st_size-512+16]),('both-headers',[512+16,disk.stat().st_size-512+16])]:
        damaged=output/(name+'.raw');shutil.copyfile(disk,damaged)
        with damaged.open('r+b') as f:
            for offset in offsets:f.seek(offset);v=f.read(1);f.seek(offset);f.write(bytes([v[0]^0xff]))
        if name=='both-headers':
            run(binary,'inspect','--source',damaged,'--json',expected=4)
        else:
            layout=run(binary,'inspect','--source',damaged,'--json')['layout'];check(len(layout['regions'])==3 and layout['warnings'],'GPT fallback warning missing')
    overlap=output/'overlap.raw';shutil.copyfile(output/'mbr-512.raw',overlap)
    with overlap.open('r+b') as f:f.seek(446+16+8);f.write(struct.pack('<I',2049))
    run(binary,'inspect','--source',overlap,'--json',expected=4)
    scan=run(binary,'scan','--source',disk,'--work-dir',output/'tamper-work','--json')
    session=json.loads(Path(scan['session']).read_text(encoding='utf-8'));session['volumes'][1]['region']['offset']+=512
    changed=output/'tampered-session.json';changed.write_text(json.dumps(session))
    run(binary,'recover','--session',changed,'--all','--destination',output/'tamper-out','--json',expected=4)
    check(not list((output/'tamper-out').glob('*')),'tampered session published data')
    native_report=native_test(binary,disk,truth,output,rows) if native else None
    result={'schema_version':1,'status':'passed','native_access':native_report,'version':call(binary,'--version'),
        'os':platform.system(),'architecture':platform.machine(),'native_virtual_disk':native,
        'source_read_only':True,'formats':35,'byte_exact':sum(r['byte_exact'] for r in rows),'results':rows,
        'partition_checks':['MBR','GPT-512','GPT-4096','protective-MBR-fallback','primary-header-fallback','primary-table-fallback','backup-header-warning','both-invalid-rejected','overlap-rejected','tampered-window-rejected','legacy-stream-same-disk-output-rejected','same-disk-scan-work-rejected','same-disk-MCP-memory-only'] if native else ['partition-image-tests']}
    (output/'results.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k!='results'}),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--native',action='store_true');a=p.parse_args()
    main(a.binary.resolve(),a.output.resolve(),a.native)
