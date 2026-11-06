"""Owned interactive-logon account for disposable native UAC test VMs only.

The random password exists only in this process. No host account is reused or
modified. The temporary administrator is deleted in finally, including on test
failure. GUI tests can load an owned profile, which is unloaded and deleted.
The returned token must be UAC-filtered.
"""
from contextlib import contextmanager
import os
import secrets


@contextmanager
def desktop_test_user():
    """Use the runner's interactive logon with administrator access removed.

    UI Automation's provider transport needs the interactive logon session,
    not merely a token with access to the same window station. No account,
    password, profile, desktop ACL, or UAC policy is changed by this fixture.
    Hosted built-in administrators have no split token: create a restricted
    copy with Administrators deny-only, privileges removed, and medium IL.
    This is a restricted-token UI test, not an actual UAC consent test.
    """
    if os.environ.get('GITHUB_ACTIONS')!='true' or os.environ.get('RUNNER_ENVIRONMENT')!='github-hosted':
        raise RuntimeError('desktop token fixture requires a disposable GitHub-hosted VM')
    import ctypes as c
    from ctypes import wintypes as w
    user=c.WinDLL('user32',use_last_error=True)
    kernel=c.WinDLL('kernel32',use_last_error=True)
    security=c.WinDLL('advapi32',use_last_error=True)
    def api(lib,name,args,result=w.BOOL):
        f=getattr(lib,name);f.argtypes=args;f.restype=result;return f
    shell=api(user,'GetShellWindow',[],w.HWND)
    owner=api(user,'GetWindowThreadProcessId',[w.HWND,c.POINTER(w.DWORD)],w.DWORD)
    current_process=api(kernel,'GetCurrentProcess',[],w.HANDLE)
    open_token=api(security,'OpenProcessToken',[w.HANDLE,w.DWORD,c.POINTER(w.HANDLE)])
    info=api(security,'GetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    sid_text=api(security,'ConvertSidToStringSidW',[c.c_void_p,c.POINTER(w.LPWSTR)])
    text_sid=api(security,'ConvertStringSidToSidW',[w.LPCWSTR,c.POINTER(c.c_void_p)])
    class SidAttributes(c.Structure):_fields_=[('sid',c.c_void_p),('attributes',w.DWORD)]
    restrict=api(security,'CreateRestrictedToken',[w.HANDLE,w.DWORD,w.DWORD,c.POINTER(SidAttributes),w.DWORD,c.c_void_p,w.DWORD,c.c_void_p,c.POINTER(w.HANDLE)])
    set_info=api(security,'SetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD])
    sid_length=api(security,'GetLengthSid',[c.c_void_p],w.DWORD)
    close=api(kernel,'CloseHandle',[w.HANDLE]);free=api(kernel,'LocalFree',[c.c_void_p],c.c_void_p)
    def checked(ok):
        if not ok:raise c.WinError(c.get_last_error())
    process=None;token=w.HANDLE();linked=w.HANDLE();sid=w.LPWSTR();admin=c.c_void_p();medium=c.c_void_p();restricted=False
    try:
        hwnd=shell()
        if not hwnd:raise RuntimeError('active desktop has no shell window')
        pid=w.DWORD();checked(owner(hwnd,c.byref(pid)))
        # Filter our own primary token. CreateProcessAsUser can assign this
        # restricted copy without SeAssignPrimaryTokenPrivilege; a copy of
        # Explorer's token belongs to another process and lacks that exemption.
        checked(open_token(current_process(),0x008b,c.byref(token)))
        kind=w.DWORD();needed=w.DWORD()
        checked(info(token,18,c.byref(kind),c.sizeof(kind),c.byref(needed)))
        if kind.value==2:
            checked(info(token,19,c.byref(linked),c.sizeof(linked),c.byref(needed)));selected=linked
        elif kind.value==3:selected=token
        elif kind.value==1:
            checked(text_sid('S-1-5-32-544',c.byref(admin)))
            disabled=SidAttributes(admin,0)
            checked(restrict(token,1,1,c.byref(disabled),0,None,0,None,c.byref(linked)))
            checked(text_sid('S-1-16-8192',c.byref(medium)))
            label=SidAttributes(medium,0x20)
            checked(set_info(linked,25,c.byref(label),c.sizeof(label)+sid_length(medium)))
            selected=linked;restricted=True
        else:raise RuntimeError(f'unexpected desktop elevation type: {kind.value}')
        data=c.create_string_buffer(256);checked(info(selected,1,data,len(data),c.byref(needed)))
        checked(sid_text(c.c_void_p.from_buffer(data),c.byref(sid)))
        if restricted:
            # A service token's default ACL may grant only Administrators and
            # SYSTEM. Once Administrators is deny-only, a child cannot access
            # objects created from that ACL and user32 initialization can fail.
            # Change only the owned token copy, never the runner's token/ACL.
            descriptor=c.c_void_p();acl=c.c_void_p();present=w.BOOL();defaulted=w.BOOL()
            parse=api(security,'ConvertStringSecurityDescriptorToSecurityDescriptorW',[w.LPCWSTR,w.DWORD,c.POINTER(c.c_void_p),c.c_void_p])
            get_acl=api(security,'GetSecurityDescriptorDacl',[c.c_void_p,c.POINTER(w.BOOL),c.POINTER(c.c_void_p),c.POINTER(w.BOOL)])
            try:
                checked(parse('D:(A;;GA;;;SY)(A;;GA;;;'+sid.value+')',1,c.byref(descriptor),None))
                checked(get_acl(descriptor,c.byref(present),c.byref(acl),c.byref(defaulted)))
                checked(set_info(selected,6,c.byref(acl),c.sizeof(acl)))
            finally:
                if descriptor:free(descriptor)
        yield {'token':selected,'sid':sid.value,'restricted':restricted,'proof':{'account':'existing runner identity','shell_pid':pid.value,'method':'restricted caller token' if restricted else 'existing limited caller token','account_modified':False,'profile_loaded':False,'uac_consent_test':False}}
    finally:
        if sid:free(c.cast(sid,c.c_void_p))
        if admin:free(admin)
        if medium:free(medium)
        for handle in [linked,token,process]:
            if handle:close(handle)


@contextmanager
def interactive_test_user(load_profile=False):
    if os.environ.get('GITHUB_ACTIONS')!='true' or os.environ.get('RUNNER_ENVIRONMENT')!='github-hosted':
        raise RuntimeError('test account requires a disposable GitHub-hosted VM')
    import ctypes as c
    from ctypes import wintypes as w
    network=c.WinDLL('netapi32',use_last_error=True)
    security=c.WinDLL('advapi32',use_last_error=True)
    kernel=c.WinDLL('kernel32',use_last_error=True)
    userenv=c.WinDLL('userenv',use_last_error=True)
    def api(lib,name,args,result=w.BOOL):
        f=getattr(lib,name);f.argtypes=args;f.restype=result;return f
    class User(c.Structure):
        _fields_=[('name',w.LPWSTR),('password',w.LPWSTR),('password_age',w.DWORD),('privilege',w.DWORD),
                  ('home',w.LPWSTR),('comment',w.LPWSTR),('flags',w.DWORD),('script',w.LPWSTR)]
    class Member(c.Structure):_fields_=[('sid',c.c_void_p)]
    class SidAttributes(c.Structure):_fields_=[('sid',c.c_void_p),('attributes',w.DWORD)]
    class Profile(c.Structure):
        _fields_=[('size',w.DWORD),('flags',w.DWORD),('username',w.LPWSTR),('path',w.LPWSTR),('default_path',w.LPWSTR),('server',w.LPWSTR),('policy',w.LPWSTR),('handle',w.HANDLE)]
    load=api(userenv,'LoadUserProfileW',[w.HANDLE,c.POINTER(Profile)])
    unload=api(userenv,'UnloadUserProfile',[w.HANDLE,w.HANDLE])
    delete_profile=api(userenv,'DeleteProfileW',[w.LPCWSTR,w.LPCWSTR,w.LPCWSTR])
    add=api(network,'NetUserAdd',[w.LPCWSTR,w.DWORD,c.POINTER(User),c.POINTER(w.DWORD)],w.DWORD)
    remove=api(network,'NetUserDel',[w.LPCWSTR,w.LPCWSTR],w.DWORD)
    group_add=api(network,'NetLocalGroupAddMembers',[w.LPCWSTR,w.LPCWSTR,w.DWORD,c.POINTER(Member),w.DWORD],w.DWORD)
    text_sid=api(security,'ConvertStringSidToSidW',[w.LPCWSTR,c.POINTER(c.c_void_p)])
    sid_text=api(security,'ConvertSidToStringSidW',[c.c_void_p,c.POINTER(w.LPWSTR)])
    lookup=api(security,'LookupAccountSidW',[w.LPCWSTR,c.c_void_p,w.LPWSTR,c.POINTER(w.DWORD),w.LPWSTR,c.POINTER(w.DWORD),c.POINTER(w.DWORD)])
    logon=api(security,'LogonUserW',[w.LPCWSTR,w.LPCWSTR,w.LPCWSTR,w.DWORD,w.DWORD,c.POINTER(w.HANDLE)])
    info=api(security,'GetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    close=api(kernel,'CloseHandle',[w.HANDLE]);free=api(kernel,'LocalFree',[c.c_void_p],c.c_void_p)
    def checked(ok):
        if not ok:raise c.WinError(c.get_last_error())
    def net_checked(code):
        if code:raise c.WinError(code)
    name='rescue-'+secrets.token_hex(5);password='Rs9!'+secrets.token_urlsafe(32)
    created=False;token=w.HANDLE();linked=w.HANDLE();admin=c.c_void_p();sid_string=w.LPWSTR();profile=Profile()
    proof={'account':'owned disposable local account','logon_type':'interactive','profile_loaded':False,'account_removed':False}
    try:
        user=User(name,password,0,1,None,'Rescue owned native test fixture',0x10001,None);parameter=w.DWORD()
        net_checked(add(None,1,c.byref(user),c.byref(parameter)));created=True
        checked(text_sid('S-1-5-32-544',c.byref(admin)))
        group=c.create_unicode_buffer(256);domain=c.create_unicode_buffer(256);group_len=w.DWORD(256);domain_len=w.DWORD(256);use=w.DWORD()
        checked(lookup(None,admin,group,c.byref(group_len),domain,c.byref(domain_len),c.byref(use)))
        # Obtain the new account's SID through a real interactive logon, then
        # close it and log on again after group membership is established.
        checked(logon(name,'.',password,2,0,c.byref(token)))
        data=c.create_string_buffer(256);needed=w.DWORD()
        checked(info(token,1,data,len(data),c.byref(needed)))
        member=Member(SidAttributes.from_buffer(data).sid)
        net_checked(group_add(None,group.value,0,c.byref(member),1))
        close(token);token=w.HANDLE()
        checked(logon(name,'.',password,2,0,c.byref(token)))
        kind=w.DWORD();checked(info(token,18,c.byref(kind),c.sizeof(kind),c.byref(needed)))
        if kind.value==2:
            checked(info(token,19,c.byref(linked),c.sizeof(linked),c.byref(needed)))
            selected=linked
        elif kind.value==3:selected=token
        else:raise AssertionError(f'interactive test administrator lacks a UAC split token: type={kind.value}')
        checked(info(selected,1,data,len(data),c.byref(needed)))
        checked(sid_text(SidAttributes.from_buffer(data).sid,c.byref(sid_string)))
        if load_profile:
            from windows_restricted import profile_privileges
            with profile_privileges():
                profile.size=c.sizeof(profile);profile.flags=1;profile.username=name
                checked(load(selected,c.byref(profile)))
            proof['profile_loaded']=True;proof['profile_removed']=False
        yield {'token':selected,'sid':sid_string.value,'proof':proof}
    finally:
        password=None
        try:
            if profile.handle:
                from windows_restricted import profile_privileges
                with profile_privileges():
                    checked(unload(selected,profile.handle))
                    checked(delete_profile(sid_string.value,None,None))
                proof['profile_removed']=True
        finally:
            for handle in [linked,token]:
                if handle:close(handle)
            if sid_string:free(c.cast(sid_string,c.c_void_p))
            if admin:free(admin)
            if created:
                code=remove(None,name);proof['account_removed']=code==0
                net_checked(code)
