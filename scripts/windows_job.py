"""Test-only lifetime boundary for an owned Windows process tree."""
import ctypes as c
from ctypes import wintypes as w


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
