"""Independently extract Exif and XMP with libheif's public C API on Linux."""
import ctypes as c


class HeifError(c.Structure):
    _fields_ = [('code',c.c_int), ('subcode',c.c_int), ('message',c.c_char_p)]


def read_metadata(path):
    lib = c.CDLL('libheif.so.1')
    ptr, u32 = c.c_void_p, c.c_uint32

    def function(name, result, *arguments):
        entry = getattr(lib,'heif_'+name)
        entry.argtypes, entry.restype = arguments, result
        return entry

    allocate = function('context_alloc',ptr)
    free = function('context_free',None,ptr)
    read = function('context_read_from_file',HeifError,ptr,c.c_char_p,ptr)
    primary = function('context_get_primary_image_handle',HeifError,ptr,c.POINTER(ptr))
    release = function('image_handle_release',None,ptr)
    count = function('image_handle_get_number_of_metadata_blocks',c.c_int,ptr,c.c_char_p)
    ids = function('image_handle_get_list_of_metadata_block_IDs',c.c_int,ptr,c.c_char_p,c.POINTER(u32),c.c_int)
    kind = function('image_handle_get_metadata_type',c.c_char_p,ptr,u32)
    size = function('image_handle_get_metadata_size',c.c_size_t,ptr,u32)
    contents = function('image_handle_get_metadata',HeifError,ptr,u32,ptr)
    context, handle = allocate(), ptr()
    assert context
    try:
        def checked(error):
            if error.code:
                raise ValueError(error.message)
        checked(read(context, bytes(path),None))
        checked(primary(context,c.byref(handle)))
        n = count(handle,None)
        assert 0 <= n <= 16
        identifiers = (u32*n)()
        assert ids(handle,None,identifiers,n) == n
        result = {}
        for identifier in identifiers:
            key = kind(handle,identifier).decode()
            length = size(handle,identifier)
            assert 0 < length <= 65536 and key not in result
            buf = c.create_string_buffer(length)
            checked(contents(handle,identifier,buf))
            data = buf.raw
            if key == 'Exif':
                offset = 4 + int.from_bytes(data[:4],'big')
                assert offset < len(data)
                data = data[offset:]
            result[key] = data
        return result
    finally:
        if handle:
            release(handle)
        free(context)
