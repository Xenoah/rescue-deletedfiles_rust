"""Temporary desktop access for an owned limited-token process on a hosted VM.

Service-logon runners can have a desktop accessible only through Administrators.
Keep its existing ACL, add the test token's own user SID without security-edit
rights, and restore/verify both original ACLs after the child exits.
"""
from contextlib import contextmanager
import hashlib
import os


@contextmanager
def temporary_desktop_access(token):
    if os.environ.get('GITHUB_ACTIONS')!='true' or os.environ.get('RUNNER_ENVIRONMENT')!='github-hosted':
        raise RuntimeError('desktop permission fixture requires a disposable GitHub-hosted VM')
    import ctypes as c
    from ctypes import wintypes as w
    user=c.WinDLL('user32',use_last_error=True);security=c.WinDLL('advapi32',use_last_error=True);kernel=c.WinDLL('kernel32',use_last_error=True)
    def api(lib,name,args,result=w.BOOL):
        f=getattr(lib,name);f.argtypes=args;f.restype=result;return f
    current=api(user,'GetProcessWindowStation',[],w.HANDLE)
    open_station=api(user,'OpenWindowStationW',[w.LPCWSTR,w.BOOL,w.DWORD],w.HANDLE)
    set_station=api(user,'SetProcessWindowStation',[w.HANDLE])
    open_desktop=api(user,'OpenDesktopW',[w.LPCWSTR,w.DWORD,w.BOOL,w.DWORD],w.HANDLE)
    close_station=api(user,'CloseWindowStation',[w.HANDLE]);close_desktop=api(user,'CloseDesktop',[w.HANDLE])
    get_sd=api(user,'GetUserObjectSecurity',[w.HANDLE,c.POINTER(w.DWORD),c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    set_sd=api(user,'SetUserObjectSecurity',[w.HANDLE,c.POINTER(w.DWORD),c.c_void_p])
    get_acl=api(security,'GetSecurityDescriptorDacl',[c.c_void_p,c.POINTER(w.BOOL),c.POINTER(c.c_void_p),c.POINTER(w.BOOL)])
    init_sd=api(security,'InitializeSecurityDescriptor',[c.c_void_p,w.DWORD])
    set_acl=api(security,'SetSecurityDescriptorDacl',[c.c_void_p,w.BOOL,c.c_void_p,w.BOOL])
    get_info=api(security,'GetTokenInformation',[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)])
    free=api(kernel,'LocalFree',[c.c_void_p],c.c_void_p)
    class SidAttributes(c.Structure):_fields_=[('Sid',c.c_void_p),('Attributes',w.DWORD)]
    class Trustee(c.Structure):_fields_=[('multiple',c.c_void_p),('operation',w.DWORD),('form',w.DWORD),('kind',w.DWORD),('name',c.c_void_p)]
    class Entry(c.Structure):_fields_=[('permissions',w.DWORD),('mode',w.DWORD),('inheritance',w.DWORD),('trustee',Trustee)]
    merge=api(security,'SetEntriesInAclW',[w.ULONG,c.POINTER(Entry),c.c_void_p,c.POINTER(c.c_void_p)],w.DWORD)
    def checked(value):
        if not value:raise c.WinError(c.get_last_error())
    information=w.DWORD(4)
    def descriptor(handle):
        needed=w.DWORD();get_sd(handle,c.byref(information),None,0,c.byref(needed))
        if not 20<=needed.value<=1024*1024:raise AssertionError('invalid desktop security descriptor size')
        data=c.create_string_buffer(needed.value)
        checked(get_sd(handle,c.byref(information),data,len(data),c.byref(needed)))
        present=w.BOOL();defaulted=w.BOOL();acl=c.c_void_p()
        checked(get_acl(data,c.byref(present),c.byref(acl),c.byref(defaulted)))
        raw=b''
        if present.value and acl:
            size=c.c_ushort.from_address(acl.value+2).value
            if not 8<=size<=65535:raise AssertionError('invalid desktop ACL size')
            raw=c.string_at(acl,size)
        return data,acl,raw
    token_data=c.create_string_buffer(256);returned=w.DWORD()
    checked(get_info(token,1,token_data,len(token_data),c.byref(returned)))
    sid=SidAttributes.from_buffer(token_data).Sid
    original_station=current();checked(original_station)
    station=None;desktop=None;changed=[];proof={'scope':'own test token user only; no WRITE_DAC/WRITE_OWNER/DELETE grant','objects':[],'restored':False}
    try:
        station=open_station('winsta0',False,0x60003);checked(station)
        checked(set_station(station))
        try:desktop=open_desktop('default',0,False,0x60081);checked(desktop)
        finally:checked(set_station(original_station))
        for name,handle,rights in [('window_station',station,0x37f),('desktop',desktop,0x1ff)]:
            original,acl,before=descriptor(handle)
            item={'name':name,'permissions':rights,'original_acl_sha256':hashlib.sha256(before).hexdigest(),'changed':False,'restored':False};proof['objects'].append(item)
            if acl:
                entry=Entry(rights,1,0,Trustee(None,0,0,1,sid));merged=c.c_void_p()
                error=merge(1,c.byref(entry),acl,c.byref(merged))
                if error:raise c.WinError(error)
                try:
                    replacement=c.create_string_buffer(64);checked(init_sd(replacement,1));checked(set_acl(replacement,True,merged,False))
                    checked(set_sd(handle,c.byref(information),replacement));item['changed']=True
                    changed.append((handle,original,before,item))
                finally:free(merged)
            else:item['restored']=True
        yield proof
    finally:
        failure=None
        for handle,original,before,item in reversed(changed):
            try:
                checked(set_sd(handle,c.byref(information),original))
                after=descriptor(handle)[2];item['restored']=before==after
                if not item['restored']:raise AssertionError('desktop ACL restoration mismatch')
            except Exception as error:failure=error
        if desktop:close_desktop(desktop)
        if station:close_station(station)
        proof['restored']=all(item['restored'] for item in proof['objects'])
        if failure:raise failure
