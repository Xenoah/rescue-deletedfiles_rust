"""Additional kernel-created Btrfs inline, sliced shared, preallocated and empty layouts."""
from pathlib import Path
from btrfs_large_samples import samples as large


def samples():
    originals=large()
    corpus=Path(__file__).resolve().parents[1]/'tests/corpus/files'
    originals.update({name:(corpus/name).read_bytes() for name in ['sample.png','sample.rtf','sample.xml']})
    originals['empty.private']=b''
    originals['sliced-reflink.bin']=originals['private.bin'][8192:8192+49152]
    originals['preallocated.bin']=bytes(12288)+b'X'*4096+bytes(49152)
    return originals
