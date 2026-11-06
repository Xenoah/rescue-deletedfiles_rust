"""Native test support: start an owned process with an actual limited token.

Never imported by the product. Silent UAC consent is changed only on disposable
GitHub-hosted Windows VMs and restored in finally; it is not an interactive test.
"""
from contextlib import contextmanager
import os, subprocess, tempfile


@contextmanager
def silent_admin_consent():
    import winreg
    if os.environ.get('GITHUB_ACTIONS')!='true' or os.environ.get('RUNNER_ENVIRONMENT')!='github-hosted':
        raise RuntimeError('silent consent profile requires a disposable GitHub-hosted VM')
    path=r'SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,path,0,winreg.KEY_READ|winreg.KEY_WRITE|winreg.KEY_WOW64_64KEY) as key:
        enabled=winreg.QueryValueEx(key,'EnableLUA')[0]
        if enabled!=1:raise RuntimeError('limited-token elevation requires EnableLUA already enabled; no reboot or global enable change is attempted')
        old,kind=winreg.QueryValueEx(key,'ConsentPromptBehaviorAdmin')
        proof={'original_admin_consent':old,'test_admin_consent':0,'enable_lua':enabled,'restored':False}
        try:
            winreg.SetValueEx(key,'ConsentPromptBehaviorAdmin',0,winreg.REG_DWORD,0)
            yield proof
        finally:
            winreg.SetValueEx(key,'ConsentPromptBehaviorAdmin',0,kind,old)
            proof['restored']=winreg.QueryValueEx(key,'ConsentPromptBehaviorAdmin')[0]==old
            if not proof['restored']:raise RuntimeError('UAC consent policy was not restored')


def run_limited(command, *, token, timeout=60, kill_tree=False):
    import ctypes as c
    from ctypes import wintypes as w
    import msvcrt
    kernel=c.WinDLL('kernel32',use_last_error=True);security=c.WinDLL('advapi32',use_last_error=True)
    class SidAttributes(c.Structure):_fields_=[('Sid',c.c_void_p),('Attributes',w.DWORD)]
    class Startup(c.Structure):
        _fields_=[('cb',w.DWORD),('lpReserved',w.LPWSTR),('lpDesktop',w.LPWSTR),('lpTitle',w.LPWSTR),
                  ('dwX',w.DWORD),('dwY',w.DWORD),('dwXSize',w.DWORD),('dwYSize',w.DWORD),
                  ('dwXCountChars',w.DWORD),('dwYCountChars',w.DWORD),('dwFillAttribute',w.DWORD),
                  ('dwFlags',w.DWORD),('wShowWindow',w.WORD),('cbReserved2',w.WORD),('lpReserved2',c.c_void_p),
                  ('hStdInput',w.HANDLE),('hStdOutput',w.HANDLE),('hStdError',w.HANDLE)]
    class Process(c.Structure):_fields_=[('hProcess',w.HANDLE),('hThread',w.HANDLE),('pid',w.DWORD),('tid',w.DWORD)]
    def api(lib,name,args,result=w.BOOL):
        f=getattr(lib,name);f.argtypes=args;f.restype=result;return f
    close=api(kernel,'CloseHandle',[w.HANDLE])
    get_info=api(security,'GetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    sub_count=api(security,'GetSidSubAuthorityCount',[c.c_void_p],c.POINTER(w.BYTE))
    sub_value=api(security,'GetSidSubAuthority',[c.c_void_p,w.DWORD],c.POINTER(w.DWORD))
    # The hosted runner is a service logon. Ask the Secondary Logon service to
    # initialize the process in the caller's session with the supplied limited
    # primary token; no alternate credentials or token elevation are involved.
    # https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-createprocesswithtokenw
    create=api(security,'CreateProcessWithTokenW',[w.HANDLE,w.DWORD,w.LPCWSTR,w.LPWSTR,w.DWORD,c.c_void_p,w.LPCWSTR,c.POINTER(Startup),c.POINTER(Process)])
    wait=api(kernel,'WaitForSingleObject',[w.HANDLE,w.DWORD],w.DWORD)
    exit_code=api(kernel,'GetExitCodeProcess',[w.HANDLE,c.POINTER(w.DWORD)])
    terminate=api(kernel,'TerminateProcess',[w.HANDLE,w.UINT])
    def checked(value):
        if not value:raise c.WinError(c.get_last_error())
    limited=token;process=Process();job=None
    try:
        elevation=w.DWORD();returned=w.DWORD()
        checked(get_info(limited,20,c.byref(elevation),c.sizeof(elevation),c.byref(returned)))
        if elevation.value:raise AssertionError('test parent token is still elevated')
        kind=w.DWORD()
        checked(get_info(limited,18,c.byref(kind),c.sizeof(kind),c.byref(returned)))
        if kind.value!=3:raise AssertionError('test parent does not have TokenElevationTypeLimited')
        buffer=c.create_string_buffer(256)
        checked(get_info(limited,25,buffer,len(buffer),c.byref(returned)))
        label=SidAttributes.from_buffer(buffer);count=sub_count(label.Sid)[0]
        if not count or sub_value(label.Sid,count-1)[0]!=8192:raise AssertionError('test parent integrity is not medium')
        from windows_desktop import temporary_desktop_access
        with temporary_desktop_access(limited) as desktop_proof:
            with open(os.devnull,'rb') as stdin,tempfile.TemporaryFile() as stdout,tempfile.TemporaryFile() as stderr:
                handles=[msvcrt.get_osfhandle(f.fileno()) for f in [stdin,stdout,stderr]]
                for handle in handles:os.set_handle_inheritable(handle,True)
                start=Startup();start.cb=c.sizeof(start);start.dwFlags=0x100
                start.lpDesktop='winsta0\\default'
                start.hStdInput,start.hStdOutput,start.hStdError=handles
                args=list(map(str,command));text=c.create_unicode_buffer(subprocess.list2cmdline(args))
                checked(create(limited,0,args[0],text,0x08000000|(4 if kill_tree else 0),None,None,c.byref(start),c.byref(process)))
                if kill_tree:
                    from windows_job import owned_process_job
                    job=owned_process_job(process.hProcess)
                    resume=api(kernel,'ResumeThread',[w.HANDLE],w.DWORD)
                    if resume(process.hThread)==0xffffffff:raise c.WinError(c.get_last_error())
                outcome=wait(process.hProcess,int(timeout*1000))
                if outcome!=0:
                    terminate(process.hProcess,124);wait(process.hProcess,5000)
                    raise subprocess.TimeoutExpired(args,timeout)
                code=w.DWORD();checked(exit_code(process.hProcess,c.byref(code)))
                stdout.seek(0);stderr.seek(0)
                result=subprocess.CompletedProcess(args,code.value,stdout.read().decode('utf-8','replace'),stderr.read().decode('utf-8','replace'))
                result.token_profile={'method':'owned_interactive_uac_linked','launcher':'CreateProcessWithTokenW','elevated':False,'integrity':'medium','elevation_type':kind.value,'desktop':'winsta0\\default','desktop_permissions':desktop_proof,'owned_tree_job':kill_tree}
                return result
    finally:
        if kill_tree and process.hProcess and not job:
            terminate(process.hProcess,124);wait(process.hProcess,5000)
        if job:close(job)
        for handle in [process.hThread,process.hProcess]:
            if handle:close(handle)
