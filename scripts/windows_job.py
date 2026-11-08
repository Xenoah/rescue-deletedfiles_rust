"""Test-only lifetime boundary for an owned Windows process tree."""
import ctypes as c
from ctypes import wintypes as w


def remaining_job_processes(job):
    """Read-only teardown evidence; keep the normal kill-on-close policy."""
    kernel=c.WinDLL('kernel32',use_last_error=True)
    kernel.QueryInformationJobObject.argtypes=[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.c_void_p]
    kernel.QueryInformationJobObject.restype=w.BOOL
    kernel.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD];kernel.OpenProcess.restype=w.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes=[w.HANDLE,w.DWORD,w.LPWSTR,c.POINTER(w.DWORD)]
    kernel.QueryFullProcessImageNameW.restype=w.BOOL
    kernel.CloseHandle.argtypes=[w.HANDLE];kernel.CloseHandle.restype=w.BOOL
    class Ids(c.Structure):
        _fields_=[('assigned',w.DWORD),('count',w.DWORD),('ids',c.c_size_t*1024)]
    ids=Ids()
    if not kernel.QueryInformationJobObject(job,3,c.byref(ids),c.sizeof(ids),None):
        return {'query_error':c.get_last_error()}
    records=[]
    for pid in ids.ids[:ids.count]:
        handle=kernel.OpenProcess(0x1000,False,pid)
        if not handle:records.append({'pid':pid,'query_error':c.get_last_error()});continue
        try:
            size=w.DWORD(32768);path=c.create_unicode_buffer(size.value)
            if kernel.QueryFullProcessImageNameW(handle,0,path,c.byref(size)):
                records.append({'pid':pid,'image':path.value})
            else:records.append({'pid':pid,'query_error':c.get_last_error()})
        finally:kernel.CloseHandle(handle)
    return {'processes':records}


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
