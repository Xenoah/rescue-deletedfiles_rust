"""Create a new 4Kn VHDX using version 2 of the Windows virtual disk API."""
import ctypes as c
from pathlib import Path
import uuid


def create(image: Path, size: int):
    if image.exists() or not image.is_absolute():
        raise ValueError('requires a new absolute test image path')
    guid = c.c_ubyte * 16

    class Storage(c.Structure):
        _fields_ = [('device', c.c_uint32), ('vendor', guid)]

    class Version2(c.Structure):
        _fields_ = [('id', guid), ('size', c.c_uint64), ('block', c.c_uint32),
                    ('logical', c.c_uint32), ('physical', c.c_uint32),
                    ('parent', c.c_wchar_p), ('source', c.c_wchar_p), ('open_flags', c.c_uint32),
                    ('parent_type', Storage), ('source_type', Storage), ('resiliency', guid)]

    class Parameters(c.Structure):
        _fields_ = [('version', c.c_uint32), ('v2', Version2)]

    vendor = guid.from_buffer_copy(uuid.UUID('ec984aec-a0f9-47e9-901f-71415a66345b').bytes_le)
    kind = Storage(3, vendor)
    params = Parameters()
    params.version = 2
    params.v2.size = size
    params.v2.block = 2 << 20
    params.v2.logical = params.v2.physical = 4096
    handle = c.c_void_p()
    create_disk = c.WinDLL('virtdisk').CreateVirtualDisk
    create_disk.argtypes = [c.POINTER(Storage), c.c_wchar_p, c.c_uint32, c.c_void_p, c.c_uint32,
                            c.c_uint32, c.POINTER(Parameters), c.c_void_p, c.POINTER(c.c_void_p)]
    create_disk.restype = c.c_uint32
    result = create_disk(c.byref(kind), str(image), 0, None, 0, 0, c.byref(params), None, c.byref(handle))
    if result:
        raise c.WinError(result)
    close = c.WinDLL('kernel32').CloseHandle
    close.argtypes = [c.c_void_p]
    if not close(handle):
        raise c.WinError()
