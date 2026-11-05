"""Independent NTFS attribute-list metadata, exact payloads and hostile references."""
import argparse, hashlib, json, struct, subprocess, tempfile
from pathlib import Path
from e2e import run, check, Client
from make_fixture import build, record, filename, resident, nonresident, MFT_LCN, SECTOR, RECORD, put


def ident(a, n):
    a=bytearray(a);put(a,14,'H',n);return a


def extent(runs,size,start=0,**kw):
    a=nonresident(runs,size,**kw);put(a,16,'Q',start);put(a,24,'Q',start+sum(n for n,_ in runs)-1);return a


def entry(kind,identifier,index,sequence=7,start=0,name=None):
    encoded=name.encode('utf-16le') if name else b''
    a=bytearray((26+len(encoded)+7)&~7);put(a,0,'I',kind);put(a,4,'H',len(a));a[6]=len(encoded)//2;a[7]=26 if encoded else 0
    put(a,8,'Q',start);put(a,16,'Q',index|(sequence<<48));put(a,24,'H',identifier);a[26:26+len(encoded)]=encoded;return a


def fixture(path,variant='normal'):
    truth=build(path);vol=bytearray(path.read_bytes())
    def save(n,attrs,base=0,seq=7,flags=0):
        r=record(n,attrs,sequence=seq,flags=flags);put(r,32,'Q',base);vol[MFT_LCN*SECTOR+n*RECORD:MFT_LCN*SECTOR+(n+1)*RECORD]=r
    references=[entry(0x30,1,30),entry(0x80,2,30),entry(0x80,3,31,start=1),entry(0x80,4,32,name='補足'),entry(0x80,5,33,start=1,name='補足')]
    list_value=b''.join(references)
    payload=b'A'*512+b'B'*512+b'C'*188
    ads=b'D'*512+b'E'*188
    base_extent=ident(extent([(1,600)],len(payload)),2)
    second=ident(extent([(1,603),(1,601)],len(payload),start=1),3)
    ext_ref=30|(7<<48);seq=7;ext_flags=0
    if variant=='wrong-sequence':seq=8
    if variant=='wrong-base':ext_ref=29|(7<<48)
    if variant=='reused-extension':ext_flags=1
    if variant=='wrong-id':put(second,14,'H',9)
    if variant=='missing-vcn':put(second,16,'Q',2);put(second,24,'Q',3);references[2]=entry(0x80,3,31,start=2);list_value=b''.join(references)
    if variant=='overlap-vcn':put(second,16,'Q',0);put(second,24,'Q',1);references[2]=entry(0x80,3,31);list_value=b''.join(references)
    if variant=='physical-overlap':second=ident(extent([(1,600),(1,601)],len(payload),start=1),3)
    if variant=='duplicate-entry':list_value+=references[2]
    if variant=='out-of-mft':references[2]=entry(0x80,3,900,start=1);list_value=b''.join(references)
    if variant=='short-entry':list_value=bytearray(list_value);put(list_value,4,'H',0)
    if variant=='unlisted':references=references[1:];list_value=b''.join(references)
    if variant=='wrong-name':references[2]=entry(0x80,3,31,start=1,name='wrong');list_value=b''.join(references)
    list_attr=ident(resident(0x20,list_value),6)
    if variant=='nonresident-list':
        list_attr=ident(nonresident([(1,620)],len(list_value)),6);put(list_attr,0,'I',0x20);vol[620*SECTOR:620*SECTOR+len(list_value)]=list_value
    save(30,[ident(filename('extended.bin'),1),list_attr,base_extent])
    ext_attrs=[second]
    if variant=='cyclic-list':ext_attrs.append(ident(resident(0x20,references[1]),10))
    save(31,ext_attrs,ext_ref,seq,ext_flags)
    save(32,[ident(extent([(1,610)],len(ads),name='補足'),4)],ext_ref)
    save(33,[ident(extent([(1,609)],len(ads),start=1,name='補足'),5)],ext_ref)
    for lcn,data in [(600,b'A'*512),(603,b'B'*512),(601,b'C'*188),(610,b'D'*512),(609,b'E'*188)]:vol[lcn*SECTOR:lcn*SECTOR+len(data)]=data
    path.write_bytes(vol)
    return {'mft-30-7':payload, 'mft-30-7-ads-'+''.join(f'{x:04x}' for x in struct.unpack('<HH','補足'.encode('utf-16le'))):ads}


def main(binary,output=None):
    count=0
    with tempfile.TemporaryDirectory(prefix='rescue-extents-') as tmp:
        root=Path(tmp)
        for variant in ['normal','nonresident-list','wrong-sequence','wrong-base','reused-extension','wrong-id','missing-vcn','overlap-vcn','physical-overlap','duplicate-entry','out-of-mft','short-entry','unlisted','wrong-name','cyclic-list']:
            source=root/(variant+'.raw');truth=fixture(source,variant);before=hashlib.sha256(source.read_bytes()).hexdigest()
            s=run(binary,'scan','--source',source,'--work-dir',root/(variant+'-work'),'--json');session=Path(s['session']);data=json.loads(session.read_text())
            found=[c for c in data['candidates'] if c['record']==30]
            if variant in ['normal','nonresident-list']:
                check(len(found)==2 and all(c['supported'] for c in found),'valid extent rejected '+str(found))
                out=root/(variant+'-output');r=run(binary,'recover','--session',session,*[v for key in truth for v in ['--id',key]],'--destination',out,'--json')
                for row in r['results']:check(Path(row['path']).read_bytes()==truth[row['candidate_id']],'extent bytes differ')
                if variant=='normal':
                    client=Client(binary,source,root/'mcp-work',root/'mcp-out')
                    try:
                        client.rpc('initialize',{'protocolVersion':'2025-11-25'});job=client.tool('start_scan',{'source':str(source),'request_id':'extent-scan'});finished=client.wait(job['job_id']);check(finished['state']=='completed',str(finished))
                        result=client.tool('list_candidates',{'session_id':finished['result']['session_id'],'limit':100})
                        check(sum(c['record']==30 and c['supported'] for c in result['candidates'])==2,'MCP extent mismatch')
                    finally:client.close()
            else:check(found and not any(c['supported'] for c in found),'malformed list accepted '+variant+str(found))
            check(hashlib.sha256(source.read_bytes()).hexdigest()==before,'source modified');count+=1
        # Bootstrap an MFT whose additional run is described by a reachable extension.
        source=root/'mft.raw';truth=build(source);vol=bytearray(source.read_bytes());tail=bytes(vol[MFT_LCN*SECTOR+16*RECORD:MFT_LCN*SECTOR+64*RECORD]);vol[800*SECTOR:800*SECTOR+len(tail)]=tail;vol[MFT_LCN*SECTOR+16*RECORD:MFT_LCN*SECTOR+64*RECORD]=b'\0'*len(tail)
        first=ident(extent([(32,MFT_LCN)],64*RECORD),1);second=ident(extent([(96,800)],64*RECORD,start=32),1)
        listing=resident(0x20,entry(0x80,1,0,sequence=1)+entry(0x80,1,1,sequence=1,start=32))
        vol[MFT_LCN*SECTOR:MFT_LCN*SECTOR+RECORD]=record(0,[first,listing],flags=1,sequence=1)
        extension=record(1,[second],flags=1,sequence=1);put(extension,32,'Q',1<<48);vol[MFT_LCN*SECTOR+RECORD:MFT_LCN*SECTOR+2*RECORD]=extension;source.write_bytes(vol)
        s=run(binary,'scan','--source',source,'--work-dir',root/'mft-work','--json');r=run(binary,'recover','--session',s['session'],'--all','--destination',root/'mft-output','--json')
        check(len(r['results'])==len(truth),'MFT candidate count')
        for row in r['results']:check(hashlib.sha256(Path(row['path']).read_bytes()).hexdigest()==truth[row['candidate_id']]['sha256'],'MFT bootstrap payload')
        count+=1
        # A compression unit and sparse stream split across records must merge before decoding.
        for compressed in [False,True]:
            source=root/('split-'+str(compressed)+'.raw');build(source);vol=bytearray(source.read_bytes());flags=1 if compressed else 0x8000
            size=8192 if compressed else 1024
            args={'flags':flags,'compression':4 if compressed else 0}
            first=ident(extent([(1,650)],size,**args),2);second=ident(extent([(15 if compressed else 1,None)],size,start=1,**args),3)
            listing=resident(0x20,entry(0x30,1,30)+entry(0x80,2,30)+entry(0x80,3,31,start=1))
            vol[MFT_LCN*SECTOR+30*RECORD:MFT_LCN*SECTOR+31*RECORD]=record(30,[ident(filename('split.bin'),1),listing,first])
            ext=record(31,[second]);put(ext,32,'Q',30|(7<<48));vol[MFT_LCN*SECTOR+31*RECORD:MFT_LCN*SECTOR+32*RECORD]=ext
            encoded=bytes([3,0xb0,2,65,0xfc,0x0f])*2 if compressed else b'A'*512
            vol[650*SECTOR:650*SECTOR+len(encoded)]=encoded;source.write_bytes(vol)
            s=run(binary,'scan','--source',source,'--work-dir',root/('split-'+str(compressed)+'-work'),'--json')
            r=run(binary,'recover','--session',s['session'],'--id','mft-30-7','--destination',root/('split-'+str(compressed)+'-out'),'--json')['results'][0]
            check(Path(r['path']).read_bytes()==(b'A'*8192 if compressed else b'A'*512+b'\0'*512),'compressed/sparse extent merge');count+=1
        # Volume allocation is a separate evidence axis, not a complete-content oracle.
        for state in ['unallocated','conflict','unknown']:
            source=root/('bitmap-'+state+'.raw');truth=build(source);vol=bytearray(source.read_bytes());bitmap=bytearray(256)
            if state=='conflict':bitmap[302//8] |= 1<<(302%8)
            if state=='unknown':bitmap=bitmap[:1]
            vol[MFT_LCN*SECTOR+6*RECORD:MFT_LCN*SECTOR+7*RECORD]=record(6,[filename('$Bitmap'),resident(0x80,bitmap)],flags=1,sequence=1);source.write_bytes(vol)
            s=run(binary,'scan','--source',source,'--work-dir',root/('bitmap-'+state+'-work'),'--json');c=run(binary,'results','--session',s['session'],'--name','fragmented.bin','--json')['candidates'][0]
            check(c['allocation_status']==state and c['supported'],'allocation is not independent evidence '+str(c))
            r=run(binary,'recover','--session',s['session'],'--id',c['id'],'--destination',root/('bitmap-'+state+'-out'),'--json')['results'][0]
            check(r['allocation_status']==state and r['sha256']==truth[c['id']]['sha256'],'allocation/recovery evidence mismatch');count+=1
        # Hard-link and DOS names remain one logical data candidate, with separately verified paths.
        source=root/'names.raw';build(source);vol=bytearray(source.read_bytes());dos=filename('REPORT~1.PDF');dos[24+65]=2
        vol[MFT_LCN*SECTOR+30*RECORD:MFT_LCN*SECTOR+31*RECORD]=record(30,[filename('report.pdf',10,2),filename('renamed.bin'),dos,resident(0x80,b'one payload, several names')]);source.write_bytes(vol)
        s=run(binary,'scan','--source',source,'--work-dir',root/'names-work','--json');found=run(binary,'results','--session',s['session'],'--name','report.pdf','--json')['candidates']
        check(len(found)==1 and found[0]['name']=='renamed.bin' and len(found[0]['alternate_names'])==2,'alias query or dedup mismatch')
        check(any(n['original_path']=='写真/report.pdf' and n['path_verified'] for n in found[0]['alternate_names']),'alias parent sequence not checked')
        filtered=run(binary,'scan','--source',source,'--work-dir',root/'names-filter','--extension','pdf','--path-prefix','写真','--max-depth',0,'--json')
        check(filtered['candidates']==1,'extension/path filters ignore alias');count+=1
    result={'status':'passed','checks':count,'version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
    if output:output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
