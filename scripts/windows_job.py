"""Test-only lifetime boundary for an owned Windows process tree."""
import ctypes as c
from ctypes import wintypes as w
import os


def owned_process_job(process):
    kernel=c.WinDLL('kernel32',use_last_error=True)
    class Basic(c.Structure):
        _fields_=[('process_time',c.c_int64),('job_time',c.c_int64),('flags',w.DWORD),
                  ('min_working_set',c.c_size_t),('max_working_set',c.c_size_t),
                  ('active_processes',w.DWORD),('affinity',c.c_size_t),('priority',w.DWORD),('scheduling',w.DWORD)]
    class Io(c.Structure):_fields_=[(name,c.c_uint64) for name in ['reads','writes','other','read_bytes','write_bytes','other_bytes']]
    class Extended(c.Structure):
        _fields_=[('basic',Basic),('io',Io),('process_memory',c.c_size_t),('job_memory',c.c_size_t),('peak_process',c.c_size_t),('peak_job',c.c_size_t)]
    kernel.CreateJobObjectW.argtypes=[c.c_void_p,w.LPCWSTR];kernel.CreateJobObjectW.restype=w.HANDLE
    kernel.SetInformationJobObject.argtypes=[w.HANDLE,c.c_int,c.c_void_p,w.DWORD];kernel.SetInformationJobObject.restype=w.BOOL
    kernel.AssignProcessToJobObject.argtypes=[w.HANDLE,w.HANDLE];kernel.AssignProcessToJobObject.restype=w.BOOL
    kernel.CloseHandle.argtypes=[w.HANDLE];kernel.CloseHandle.restype=w.BOOL
    job=kernel.CreateJobObjectW(None,None)
    if not job:raise c.WinError(c.get_last_error())
    try:
        limits=Extended();limits.basic.flags=0x2000 # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not kernel.SetInformationJobObject(job,9,c.byref(limits),c.sizeof(limits)):raise c.WinError(c.get_last_error())
        if not kernel.AssignProcessToJobObject(job,process):raise c.WinError(c.get_last_error())
        return job
    except BaseException:
        kernel.CloseHandle(job)
        raise


def preserve_completed_input_services(job):
    """After the GUI driver exits, retain only recognized Windows input hosts.

    KILL_ON_JOB_CLOSE remains active during the test and on every failure.
    A shared IME broker can be created as a descendant of the first GUI; killing
    it at normal fixture teardown can leave subsequent TSF clients with a stale
    RPC endpoint. Unknown or application-owned survivors retain normal cleanup.
    """
    kernel=c.WinDLL('kernel32',use_last_error=True)
    kernel.QueryInformationJobObject.argtypes=[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.c_void_p]
    kernel.QueryInformationJobObject.restype=w.BOOL
    kernel.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD];kernel.OpenProcess.restype=w.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes=[w.HANDLE,w.DWORD,w.LPWSTR,c.POINTER(w.DWORD)]
    kernel.QueryFullProcessImageNameW.restype=w.BOOL
    kernel.CloseHandle.argtypes=[w.HANDLE];kernel.CloseHandle.restype=w.BOOL
    class ProcessIds(c.Structure):
        _fields_=[('assigned',w.DWORD),('count',w.DWORD),('ids',c.c_size_t*1024)]
    ids=ProcessIds()
    if not kernel.QueryInformationJobObject(job,3,c.byref(ids),c.sizeof(ids),None):
        return {'preserved':False,'query_error':c.get_last_error()}
    records=[]
    for pid in ids.ids[:ids.count]:
        handle=kernel.OpenProcess(0x1000,False,pid)
        if not handle:
            records.append({'pid':pid,'query_error':c.get_last_error()});continue
        try:
            size=w.DWORD(32768);path=c.create_unicode_buffer(size.value)
            if not kernel.QueryFullProcessImageNameW(handle,0,path,c.byref(size)):
                records.append({'pid':pid,'query_error':c.get_last_error()});continue
            records.append({'pid':pid,'image':path.value})
        finally:kernel.CloseHandle(handle)
    windows=os.path.normcase(os.path.abspath(os.environ['SystemRoot']))+os.sep
    input_hosts={'ctfmon.exe','imebroker.exe','microsoftime.exe','textinputhost.exe'}
    companions={'runtimebroker.exe','dllhost.exe','conhost.exe'}
    def permitted(record):
        path=os.path.normcase(record.get('image',''))
        return path.startswith(windows) and os.path.basename(path) in input_hosts|companions
    proof={'preserved':False,'survivors':records}
    if not records or not all(permitted(r) for r in records):return proof
    if not any(os.path.basename(os.path.normcase(r['image'])) in input_hosts for r in records):return proof
    # Query and preserve the original limits, changing only kill-on-close after
    # every test-owned executable has exited. The OS hosts keep their own token.
    limits=c.create_string_buffer(512);returned=w.DWORD()
    if not kernel.QueryInformationJobObject(job,9,limits,len(limits),c.byref(returned)):
        raise c.WinError(c.get_last_error())
    flags=w.DWORD.from_buffer(limits,16);flags.value &= ~0x2000
    kernel.SetInformationJobObject.argtypes=[w.HANDLE,c.c_int,c.c_void_p,w.DWORD]
    kernel.SetInformationJobObject.restype=w.BOOL
    if not kernel.SetInformationJobObject(job,9,limits,returned.value):raise c.WinError(c.get_last_error())
    proof['preserved']=True
    return proof
