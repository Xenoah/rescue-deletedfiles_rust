"""Actual Windows UI Automation from fresh settings to selected ZIP recovery.

This initial suite verifies UIA actions and names, not a physical keyboard, a
screen-reader product, or an installed Japanese IME. Only its own process and
regular fixture files are touched. Failed-tree evidence is kept for diagnosis.
"""
import argparse,json,os,platform,subprocess,tempfile,zipfile,time,threading
from contextlib import nullcontext
from pathlib import Path
from e2e import check
from e2e_formats import corpus_load,ntfs_image,file_hash


def main(binary,output,limited=False,desktop=False,keyboard=False,limited_gui=False,scheduled=False,ime=False):
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
    config.write_text(json.dumps({'binary':str(binary),'source':str(source),'destination':str(destination),'root':str(root),'result':str(result),'pdf':'削除した報告書.pdf','text':'原文.txt','query':'原文','keyboard':keyboard,'ime':ime,'require_nonadmin':limited or scheduled,'require_uac':limited and not desktop},ensure_ascii=False),encoding='utf-8')
    powershell=Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'
    check(powershell.is_file(),'system PowerShell executable is missing')
    command=[str(powershell),'-Mta','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(Path(__file__).with_name('gui_windows.ps1')),'-Config',str(config)]
    if limited:
        from windows_test_user import interactive_test_user,desktop_test_user
        from windows_restricted import run_limited
    with (desktop_test_user() if desktop else interactive_test_user(load_profile=True)) if limited else nullcontext(None) as identity:
        if limited or scheduled:
            if limited:subprocess.run(['icacls',str(root),'/grant','*'+identity['sid']+':(OI)(CI)M'],check=True,capture_output=True)
            if limited_gui or scheduled:
                # Keep the UIA driver in the runner's interactive logon. The
                # product alone inherits the real UAC-linked limited token.
                # A different-logon UIA client cannot access even an owned WPF
                # provider on this hosted desktop; that is a driver limitation.
                launcher=root/'launch.ps1';pid_file=root/'owned-pid.txt'
                launcher.write_text("param([string]$Config)\n$ErrorActionPreference='Stop'\n$c=Get-Content -LiteralPath $Config -Raw -Encoding UTF8|ConvertFrom-Json\n$p=Start-Process -FilePath $c.binary -ArgumentList @('--language','en') -PassThru -RedirectStandardOutput (Join-Path $c.root 'stdout.txt') -RedirectStandardError (Join-Path $c.root 'stderr.txt')\n$p.Id|Set-Content -LiteralPath (Join-Path $c.root 'owned-pid.txt') -Encoding UTF8\n$p.WaitForExit()\nexit $p.ExitCode\n",encoding='utf-8-sig')
                outcomes=[];errors=[]
                def launch():
                    try:
                        launch_command=[str(powershell),'-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(launcher),'-Config',str(config)]
                        if scheduled:
                            task_config=root/'task.json';task_config.write_text(json.dumps({'executable':str(powershell),'arguments':subprocess.list2cmdline(launch_command[1:]),'root':str(root)}),encoding='utf-8')
                            outcomes.append(subprocess.run([str(powershell),'-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(Path(__file__).with_name('windows_scheduled.ps1')),'-Config',str(task_config)],capture_output=True,text=True,timeout=200))
                        else:outcomes.append(run_limited(launch_command,token=identity['token'],timeout=180,kill_tree=True,user_environment=True))
                    except Exception as error:errors.append(error)
                thread=threading.Thread(target=launch);thread.start()
                try:
                    deadline=time.monotonic()+45
                    while not pid_file.exists():
                        if not thread.is_alive() or time.monotonic()>deadline:raise RuntimeError('limited GUI launcher failed: '+str(errors or outcomes))
                        time.sleep(.1)
                    cfg=json.loads(config.read_text(encoding='utf-8'));cfg['owned_pid']=int(pid_file.read_text(encoding='utf-8-sig').strip());config.write_text(json.dumps(cfg,ensure_ascii=False),encoding='utf-8')
                    completed=subprocess.run(command,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=120)
                finally:thread.join(timeout=185)
                check(not thread.is_alive() and not errors and len(outcomes)==1,'limited GUI launcher did not exit: '+str(errors))
                completed.token_profile=getattr(outcomes[0],'token_profile',{'method':'Task Scheduler least-privilege interactive token'})
                completed.token_profile['uia_driver']='runner token; only the GUI uses the limited token'
            else:
                completed=run_limited(command,token=identity['token'],timeout=180,kill_tree=True,restricted=identity.get('restricted',False),user_environment=identity['proof']['profile_loaded'])
        else:
            completed=subprocess.run(command,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=180)
    tree=json.loads(result.read_text(encoding='utf-8-sig')) if result.exists() else {'status':'no_result'}
    output.write_text(json.dumps({'status':'failed','exit':completed.returncode,'uia':tree,'stdout':completed.stdout,'stderr':completed.stderr},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    gui_log=(root/'stderr.txt').read_text(encoding='utf-8',errors='replace') if (root/'stderr.txt').exists() else '(no GUI log)'
    executable=binary.read_bytes()
    tree['native_provider_linked']={'accesskit_property_name':'AccessKitAdapter'.encode('utf-16-le') in executable,'uia_return_raw_element_provider':b'UiaReturnRawElementProvider' in executable}
    check(completed.returncode==0 and tree['status']=='passed',json.dumps(tree,ensure_ascii=False)[-7000:]+'\n'+completed.stdout+completed.stderr+'\nGUI: '+gui_log[-3000:])
    expected={'全形式/'+item['path'].name:item['data'] for item in truth.values() if item['path'].name in ['削除した報告書.pdf','原文.txt']}
    # The independent NTFS builder uses the fixed folder name 全形式.
    check(len(expected)==2,'missing GUI truth files')
    with zipfile.ZipFile(destination) as archive:
        check(archive.testzip() is None,'GUI ZIP CRC failure')
        check(set(archive.namelist())==set(expected)|{'rescue-report.json'},'unexpected selected files: '+str(archive.namelist()))
        for name,data in expected.items():check(archive.read(name)==data,'GUI bytes differ: '+name)
    check(file_hash(source)==before,'GUI changed its source')
    proof={'status':'passed','gui_binary_sha256':file_hash(binary),'source_sha256':before,'source_unchanged':True,'files_byte_exact':2,'bytes_exact':sum(map(len,expected.values())),'uia':tree,'limited_token':getattr(completed,'token_profile',None),'test_account':identity['proof'] if identity else None,'screenshot':{'file':str(root/'windows-gui-complete.png'),'sha256':file_hash(root/'windows-gui-complete.png')},'scope':__doc__.strip()}
    output.write_text(json.dumps(proof,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps({'status':'passed','files_byte_exact':2,'bytes_exact':proof['bytes_exact'],'uia_steps':len(tree['steps'])}))

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--limited',action='store_true');p.add_argument('--desktop-token',action='store_true');p.add_argument('--keyboard',action='store_true');p.add_argument('--limited-gui',action='store_true');p.add_argument('--scheduled-gui',action='store_true');p.add_argument('--ime',action='store_true');a=p.parse_args();main(a.binary.resolve(),a.output.resolve(),a.limited or a.desktop_token or a.limited_gui,a.desktop_token,a.keyboard or a.ime,a.limited_gui,a.scheduled_gui,a.ime)
