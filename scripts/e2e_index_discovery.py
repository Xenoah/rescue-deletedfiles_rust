"""Whole-source and recovered-region identities must not be interchanged."""
import argparse,json,subprocess,tempfile
from pathlib import Path
from e2e_discovery import verify,ntfs,fat32,exfat,file_hash

def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-index-discovery-') as tmp:
        root=Path(tmp)
        for kind,make in [('ntfs',ntfs),('fat32',fat32),('exfat',exfat)]:
            for broken in [False,True]:
                name=kind+('-backup' if broken else '-primary')
                _,_,_,count=verify(binary,root,name,kind,make,broken,mcp=True,indexed=True)
                exact+=count*2;checks.append(name+'_cli_mcp_bytes')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
            'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,
            'source_unchanged':True,'scope':'Synthetic lost MBR and recovered primary/backup NTFS, FAT32 and exFAT regions; both CLI and MCP indexed recovery.'}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
