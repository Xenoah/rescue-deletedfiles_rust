"""Seven independent, format-valid originals with alternating allocated and zero ranges."""
import io
import struct
import tarfile
import wave
import zipfile


def samples():
    band = 4096
    data = b''.join(bytes([(k*7+11) % 256]) * band if k%3 == 0 else bytes(band) for k in range(48))
    mono = io.BytesIO()
    with wave.open(mono, 'wb') as sound:
        sound.setnchannels(1)
        sound.setsampwidth(2)
        sound.setframerate(22050)
        sound.writeframes(data)
    bmp = struct.pack('<2sIHHI',b'BM',54+len(data),0,0,54)+struct.pack('<IiiHHIIiiII',40,256,256,1,24,0,len(data),0,0,0,0)+data
    archive = io.BytesIO()
    with zipfile.ZipFile(archive,'w') as container:
        entry = zipfile.ZipInfo('extent-data.bin',(2020,1,2,3,4,6))
        entry.create_system = 3
        container.writestr(entry,data)
    tape = io.BytesIO()
    with tarfile.open(fileobj=tape,mode='w',format=tarfile.USTAR_FORMAT) as container:
        entry=tarfile.TarInfo('extent-data.bin');entry.size=len(data)
        container.addfile(entry,io.BytesIO(data))
    return {'bands.wav':mono.getvalue(),'bands.bmp':bmp,'gray.pgm':b'P5\n256 768\n255\n'+data,
            'color.ppm':b'P6\n256 256\n255\n'+data,'bundle.zip':archive.getvalue(),'bundle.tar':tape.getvalue(),
            'measurements.dat':data}


def write_sparse(path, data, block):
    with path.open('wb') as target:
        for offset in range(0,len(data),block):
            part=data[offset:offset+block]
            if any(part):
                target.seek(offset)
                target.write(part)
        target.truncate(len(data))
