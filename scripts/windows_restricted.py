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


def run_limited(command, timeout=60):
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
    close=api(kernel,'CloseHandle',[w.HANDLE]);free=api(kernel,'LocalFree',[c.c_void_p],c.c_void_p)
    current=api(kernel,'GetCurrentProcess',[],w.HANDLE)
    open_token=api(security,'OpenProcessToken',[w.HANDLE,w.DWORD,c.POINTER(w.HANDLE)])
    get_info=api(security,'GetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    sid_from_text=api(security,'ConvertStringSidToSidW',[w.LPCWSTR,c.POINTER(c.c_void_p)])
    restrict=api(security,'CreateRestrictedToken',[w.HANDLE,w.DWORD,w.DWORD,c.POINTER(SidAttributes),w.DWORD,c.c_void_p,w.DWORD,c.c_void_p,c.POINTER(w.HANDLE)])
    set_info=api(security,'SetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD])
    sid_length=api(security,'GetLengthSid',[c.c_void_p],w.DWORD)
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
    token=w.HANDLE();limited=w.HANDLE();process=Process();admin=c.c_void_p();medium=c.c_void_p()
    try:
        checked(open_token(current(),0x0002|0x0008|0x0001|0x0080, c.byref(token)))
        elevation=w.DWORD();returned=w.DWORD()
        # Use Windows' actual UAC-linked limited token. A manually restricted
        # token has different desktop/elevation behavior and is not the token
        # used by a normal unelevated application of the same administrator.
        linked=bool(get_info(token,19,c.byref(limited),c.sizeof(limited),c.byref(returned)))
        if not linked:
            if c.get_last_error()!=1312:raise c.WinError(c.get_last_error())
            # Service-logon hosted runners have no UAC-linked interactive token.
            # Exercise an actual restricted medium token on the interactive
            # desktop, and record this distinction instead of claiming a normal
            # interactive user's consent. The denial/grant checks remain exact.
            checked(sid_from_text('S-1-5-32-544',c.byref(admin)));deny=SidAttributes(admin,0)
            checked(restrict(token,0x1|0x4,1,c.byref(deny),0,None,0,None,c.byref(limited)))
            checked(sid_from_text('S-1-16-8192',c.byref(medium)));length=sid_length(medium)
            label_buffer=c.create_string_buffer(c.sizeof(SidAttributes)+length)
            address=c.addressof(label_buffer)+c.sizeof(SidAttributes);c.memmove(address,medium,length)
            label=SidAttributes.from_buffer(label_buffer);label.Sid=address;label.Attributes=0x20
            checked(set_info(limited,25,label_buffer,len(label_buffer)))
        checked(get_info(limited,20,c.byref(elevation),c.sizeof(elevation),c.byref(returned)))
        if elevation.value:raise AssertionError('test parent token is still elevated')
        kind=w.DWORD()
        checked(get_info(limited,18,c.byref(kind),c.sizeof(kind),c.byref(returned)))
        if linked and kind.value!=3:raise AssertionError('test parent does not have TokenElevationTypeLimited')
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
                checked(create(limited,0,args[0],text,0x08000000,None,None,c.byref(start),c.byref(process)))
                outcome=wait(process.hProcess,int(timeout*1000))
                if outcome!=0:
                    terminate(process.hProcess,124);wait(process.hProcess,5000)
                    raise subprocess.TimeoutExpired(args,timeout)
                code=w.DWORD();checked(exit_code(process.hProcess,c.byref(code)))
                stdout.seek(0);stderr.seek(0)
                result=subprocess.CompletedProcess(args,code.value,stdout.read().decode('utf-8','replace'),stderr.read().decode('utf-8','replace'))
                result.token_profile={'method':'uac_linked' if linked else 'restricted_service_logon','launcher':'CreateProcessWithTokenW','elevated':False,'integrity':'medium','elevation_type':kind.value,'desktop':'winsta0\\default','desktop_permissions':desktop_proof}
                return result
    finally:
        for handle in [process.hThread,process.hProcess,limited,token]:
            if handle:close(handle)
        for pointer in [admin,medium]:
            if pointer:free(pointer)
