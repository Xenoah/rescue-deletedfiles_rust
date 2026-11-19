"""Deterministic, format-valid Btrfs reference files that exceed the inline extent size."""
import io
import random
import struct
import wave
import zipfile


def samples():
    text=''.join(f'{i:07d}  削除前の計測結果\n' for i in range(6000)).encode()
    table=b'index,square\r\n'+b''.join(f'{i},{i*i}\r\n'.encode() for i in range(12000))
    width,height=128,256
    pixels=bytes((i//32)%256 for i in range(width*height*3))
    bmp=struct.pack('<2sIHHI',b'BM',54+len(pixels),0,0,54)+struct.pack('<IiiHHIIiiII',40,width,height,1,24,0,len(pixels),0,0,0,0)+pixels
    sound=io.BytesIO()
    with wave.open(sound,'wb') as wav:
        wav.setnchannels(1);wav.setsampwidth(2);wav.setframerate(22050)
        wav.writeframes(b''.join(struct.pack('<h',(i%4096)*8-16384) for i in range(80000)))
    archive=io.BytesIO()
    with zipfile.ZipFile(archive,'w') as z:z.writestr(zipfile.ZipInfo('samples.csv',(2020,1,2,3,4,6)),table)
    return {'log.txt':text,'readings.csv':table,'image.bmp':bmp,'tone.wav':sound.getvalue(),
            'bundle.zip':archive.getvalue(),'private.bin':random.Random(371001).randbytes(153600),
            'sparse.dat':b'F'*4096+bytes(1024*1024)+b'Z'*4096}
