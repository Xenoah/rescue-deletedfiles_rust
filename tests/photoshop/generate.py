"""Generate composite and layered PSD/PSB originals with ImageMagick and psd-tools."""
from pathlib import Path
import subprocess
import struct
from PIL import Image
from psd_tools import PSDImage
from psd_tools.constants import Compression

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT.parent/'corpus/files/sample.bmp'


def generate():
    for name, mode, compression, size in [
        ('gray-zip.psd','L',Compression.ZIP,(19,13)),
        ('rgb-zip.psd','RGB',Compression.ZIP,(31,17)),
        ('rgba-prediction.psd','RGBA',Compression.ZIP_WITH_PREDICTION,(21,25)),
        ('cmyk-prediction.psd','CMYK',Compression.ZIP_WITH_PREDICTION,(23,11)),
        ('mono-raw.psd','1',Compression.RAW,(24,19)),
        ('rgb-prediction.psb','RGB',Compression.ZIP_WITH_PREDICTION,(13,27)),
    ]:
        image = Image.open(SOURCE).resize(size).convert(mode)
        doc = PSDImage.frompil(image,compression=compression)
        if name.endswith('.psb'):
            # The encoder takes its length-field and row-table widths from version.
            header=doc._record.header
            channels=doc._record.image_data.get_data(header)
            header.version=2
            doc._record.image_data.set_data(channels,header)
        doc.save(ROOT/name)
    floating = PSDImage.frompil(Image.open(SOURCE).convert('RGB').resize((23,17)),
                                compression=Compression.ZIP_WITH_PREDICTION)
    header = floating._record.header
    values = floating._record.image_data.get_data(header)
    header.depth = 32
    floating._record.image_data.set_data([b''.join(struct.pack('>f',v/255.0) for v in plane) for plane in values],header)
    floating.save(ROOT/'float32-prediction.psd')
    for name, flags, compression in [
        ('rgb-raw.psd',[], 'None'),
        ('gray-rle.psd',['-colorspace','Gray'],'RLE'),
        ('layers-rle.psb',[str(SOURCE),str(SOURCE),'-rotate','90'],'RLE'),
        ('rgb-raw.psb',['-resize','21x19!'],'None'),
        ('cmyk-rle.psb',['-colorspace','CMYK','-resize','18x14!'],'RLE'),
        ('rgb16-rle.psd',['-resize','47x27!','-depth','16'],'RLE'),
    ]:
        subprocess.run(['convert',str(SOURCE),*flags,'-compress',compression,str(ROOT/name)],check=True)


if __name__=='__main__':
    generate()
