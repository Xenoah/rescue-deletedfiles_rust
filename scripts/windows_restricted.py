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


@contextmanager
def profile_privileges():
    """Temporarily enable existing backup/restore privileges for our test profile."""
    import ctypes as c
    from ctypes import wintypes as w
    kernel=c.WinDLL('kernel32',use_last_error=True);security=c.WinDLL('advapi32',use_last_error=True)
    class Luid(c.Structure):_fields_=[('low',w.DWORD),('high',w.LONG)]
    class Privilege(c.Structure):_fields_=[('id',Luid),('attributes',w.DWORD)]
    class Privileges(c.Structure):_fields_=[('count',w.DWORD),('values',Privilege*2)]
    kernel.GetCurrentProcess.restype=w.HANDLE
    security.OpenProcessToken.argtypes=[w.HANDLE,w.DWORD,c.POINTER(w.HANDLE)]
    security.LookupPrivilegeValueW.argtypes=[w.LPCWSTR,w.LPCWSTR,c.POINTER(Luid)]
    security.AdjustTokenPrivileges.argtypes=[w.HANDLE,w.BOOL,c.POINTER(Privileges),w.DWORD,c.POINTER(Privileges),c.POINTER(w.DWORD)]
    kernel.CloseHandle.argtypes=[w.HANDLE]
    token=w.HANDLE();requested=Privileges();previous=Privileges();size=w.DWORD();changed=False
    def checked(ok):
        if not ok:raise c.WinError(c.get_last_error())
    try:
        checked(security.OpenProcessToken(kernel.GetCurrentProcess(),0x28,c.byref(token)))
        requested.count=2
        for i,name in enumerate(['SeBackupPrivilege','SeRestorePrivilege']):
            checked(security.LookupPrivilegeValueW(None,name,c.byref(requested.values[i].id)));requested.values[i].attributes=2
        c.set_last_error(0)
        checked(security.AdjustTokenPrivileges(token,False,c.byref(requested),c.sizeof(previous),c.byref(previous),c.byref(size)))
        changed=True
        if c.get_last_error()==1300:raise RuntimeError('profile fixture privileges are unavailable')
        yield
    finally:
        if changed:checked(security.AdjustTokenPrivileges(token,False,c.byref(previous),0,None,None))
        if token:kernel.CloseHandle(token)


def run_limited(command, *, token, timeout=60, kill_tree=False, restricted=False, user_environment=False):
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
    create_as_user=api(security,'CreateProcessAsUserW',[w.HANDLE,w.LPCWSTR,w.LPWSTR,c.c_void_p,c.c_void_p,w.BOOL,w.DWORD,c.c_void_p,w.LPCWSTR,c.POINTER(Startup),c.POINTER(Process)])
    wait=api(kernel,'WaitForSingleObject',[w.HANDLE,w.DWORD],w.DWORD)
    exit_code=api(kernel,'GetExitCodeProcess',[w.HANDLE,c.POINTER(w.DWORD)])
    terminate=api(kernel,'TerminateProcess',[w.HANDLE,w.UINT])
    def checked(value):
        if not value:raise c.WinError(c.get_last_error())
    limited=token;process=Process();job=None;environment=c.c_void_p()
    try:
        elevation=w.DWORD();returned=w.DWORD()
        checked(get_info(limited,20,c.byref(elevation),c.sizeof(elevation),c.byref(returned)))
        if elevation.value and not restricted:raise AssertionError('test parent token is still elevated')
        kind=w.DWORD()
        checked(get_info(limited,18,c.byref(kind),c.sizeof(kind),c.byref(returned)))
        if kind.value!=3 and not restricted:raise AssertionError('test parent does not have TokenElevationTypeLimited')
        if restricted:
            # Verify effective membership, rather than pretending a synthetic
            # restricted token has a UAC-linked TokenElevationTypeLimited.
            duplicate=api(security,'DuplicateToken',[w.HANDLE,c.c_int,c.POINTER(w.HANDLE)])
            sid_from_text=api(security,'ConvertStringSidToSidW',[w.LPCWSTR,c.POINTER(c.c_void_p)])
            membership=api(security,'CheckTokenMembership',[w.HANDLE,c.c_void_p,c.POINTER(w.BOOL)])
            free=api(kernel,'LocalFree',[c.c_void_p],c.c_void_p)
            impersonation=w.HANDLE();admin=c.c_void_p()
            try:
                # IsTokenRestricted tests a restricting SID list only; our
                # deny-only groups/removed privileges use TokenHasRestrictions.
                filtered=w.DWORD();checked(get_info(limited,21,c.byref(filtered),c.sizeof(filtered),c.byref(returned)))
                if not filtered.value:raise AssertionError('desktop token was not filtered')
                checked(duplicate(limited,2,c.byref(impersonation)))
                checked(sid_from_text('S-1-5-32-544',c.byref(admin)))
                enabled=w.BOOL();checked(membership(impersonation,admin,c.byref(enabled)))
                if enabled.value:raise AssertionError('restricted UI test still has administrator membership')
            finally:
                if impersonation:close(impersonation)
                if admin:free(admin)
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
                flags=0x08000000|(4 if kill_tree else 0)
                if user_environment:
                    userenv=c.WinDLL('userenv',use_last_error=True)
                    create_environment=api(userenv,'CreateEnvironmentBlock',[c.POINTER(c.c_void_p),w.HANDLE,w.BOOL])
                    checked(create_environment(c.byref(environment),limited,False));flags|=0x400
                if restricted:
                    # Preserve the shell logon's session for a filtered copy.
                    # Secondary Logon refuses this restricted primary token;
                    # CreateProcessAsUser explicitly supports restricted tokens.
                    checked(create_as_user(limited,args[0],text,None,None,True,flags,environment,None,c.byref(start),c.byref(process)))
                else:
                    checked(create(limited,0,args[0],text,flags,environment,None,c.byref(start),c.byref(process)))
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
                result.token_profile={'method':'restricted_desktop_token' if restricted else 'owned_interactive_uac_linked','launcher':'CreateProcessAsUserW' if restricted else 'CreateProcessWithTokenW','elevated':bool(elevation.value),'administrator_sid_enabled':False,'integrity':'medium','elevation_type':kind.value,'desktop':'winsta0\\default','desktop_permissions':desktop_proof,'owned_tree_job':kill_tree}
                if job:
                    from windows_job import remaining_job_processes
                    result.token_profile['remaining_job_processes']=remaining_job_processes(job)
                return result
    finally:
        if environment:
            destroy=api(userenv,'DestroyEnvironmentBlock',[c.c_void_p]);destroy(environment)
        if kill_tree and process.hProcess and not job:
            terminate(process.hProcess,124);wait(process.hProcess,5000)
        if job:close(job)
        for handle in [process.hThread,process.hProcess]:
            if handle:close(handle)
