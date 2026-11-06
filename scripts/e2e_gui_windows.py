"""Actual Windows UI Automation from fresh settings to selected ZIP recovery.

This initial suite verifies UIA actions and names, not a physical keyboard, a
screen-reader product, or an installed Japanese IME. Only its own process and
regular fixture files are touched. Failed-tree evidence is kept for diagnosis.
"""
import argparse,json,platform,subprocess,tempfile,zipfile
from contextlib import nullcontext
from pathlib import Path
from e2e import check
from e2e_formats import corpus_load,ntfs_image,file_hash


def main(binary,output,limited=False):
    check(platform.system()=='Windows','native Windows GUI test requires Windows')
    output.parent.mkdir(parents=True,exist_ok=True)
    # Keep failed UIA trees and logs next to the result; all names are unique.
    root=Path(tempfile.mkdtemp(prefix='rescue-owned-uia-',dir=output.parent)).resolve()
    source=root/'source.raw';destination=root/'recovered.zip';config=root/'config.json';result=root/'uia.json'
    corpus=list(corpus_load(Path(__file__).resolve().parent.parent/'tests/corpus'))
    for entry in corpus:
        if entry['path'].name=='sample.pdf':entry['path']=entry['path'].with_name('削除した報告書.pdf')
        if entry['path'].name=='sample.txt':entry['path']=entry['path'].with_name('原文.txt')
    truth,_=ntfs_image(source,corpus,'fragmented');before=file_hash(source)
    config.write_text(json.dumps({'binary':str(binary),'source':str(source),'destination':str(destination),'root':str(root),'result':str(result),'pdf':'削除した報告書.pdf','text':'原文.txt','query':'原文'},ensure_ascii=False),encoding='utf-8')
    command=['powershell.exe','-Mta','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(Path(__file__).with_name('gui_windows.ps1')),'-Config',str(config)]
    if limited:
        from windows_test_user import interactive_test_user
        from windows_restricted import run_limited
    with interactive_test_user() if limited else nullcontext(None) as identity:
        if limited:
            subprocess.run(['icacls',str(root),'/grant','*'+identity['sid']+':(OI)(CI)M'],check=True,capture_output=True)
            completed=run_limited(command,token=identity['token'],timeout=180,kill_tree=True)
        else:
            completed=subprocess.run(command,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=180)
    tree=json.loads(result.read_text(encoding='utf-8-sig')) if result.exists() else {'status':'no_result'}
    output.write_text(json.dumps({'status':'failed','uia':tree,'stdout':completed.stdout,'stderr':completed.stderr},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    check(completed.returncode==0 and tree['status']=='passed',json.dumps(tree,ensure_ascii=False)[-7000:]+'\n'+completed.stdout+completed.stderr)
    expected={'全形式/'+item['path'].name:item['data'] for item in truth.values() if item['path'].name in ['削除した報告書.pdf','原文.txt']}
    # The independent NTFS builder uses the fixed folder name 全形式.
    check(len(expected)==2,'missing GUI truth files')
    with zipfile.ZipFile(destination) as archive:
        check(archive.testzip() is None,'GUI ZIP CRC failure')
        check(set(archive.namelist())==set(expected)|{'rescue-report.json'},'unexpected selected files: '+str(archive.namelist()))
        for name,data in expected.items():check(archive.read(name)==data,'GUI bytes differ: '+name)
    check(file_hash(source)==before,'GUI changed its source')
    proof={'status':'passed','gui_binary_sha256':file_hash(binary),'source_sha256':before,'source_unchanged':True,'files_byte_exact':2,'bytes_exact':sum(map(len,expected.values())),'uia':tree,'limited_token':getattr(completed,'token_profile',None),'test_account':identity['proof'] if identity else None,'scope':__doc__.strip()}
    output.write_text(json.dumps(proof,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps({'status':'passed','files_byte_exact':2,'bytes_exact':proof['bytes_exact'],'uia_steps':len(tree['steps'])}))

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--limited',action='store_true');a=p.parse_args();main(a.binary.resolve(),a.output.resolve(),a.limited)
