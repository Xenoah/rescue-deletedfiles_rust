"""Re-encode genuine GIMP layers with psd-tools, preserving each channel's pixels."""
from pathlib import Path
from psd_tools import PSDImage
from psd_tools.constants import Compression
from psd_tools.psd.layer_and_mask import LayerInfo, LayerInfoBlock
from psd_tools.psd.tagged_blocks import TaggedBlocks, TaggedBlock
from psd_tools.constants import Tag

ROOT = Path(__file__).resolve().parent


def generate():
    for source, destination, codec, nested in [
        ('gimp-gray.psd', 'gray-layers-zip.psd', Compression.ZIP, False),
        ('gimp-mask.psd', 'mask-prediction.psd', Compression.ZIP_WITH_PREDICTION, False),
        ('gimp-layers.psd', 'rgb-layers-zip.psb', Compression.ZIP, False),
        ('gimp-high-depth.psd', 'deep-layers-raw.psb', Compression.RAW, False),
        ('gimp-high-depth.psd', 'deep-tag-prediction.psd', Compression.ZIP_WITH_PREDICTION, True),
    ]:
        doc = PSDImage.open(ROOT / source)
        header = doc._record.header
        version = 2 if destination.endswith('.psb') else 1
        composite = doc._record.image_data.get_data(header)
        info = doc._record.layer_and_mask_information
        layer_info = info.layer_info
        for record, channels in zip(layer_info.layer_records, layer_info.channel_image_data, strict=True):
            for channel, size in zip(channels, record.channel_sizes, strict=True):
                samples = channel.get_data(*size,header.depth,header.version)
                channel.compression = codec
                channel.set_data(samples,*size,header.depth,version)
        header.version = version
        doc._record.image_data.set_data(composite,header)
        if nested:
            info.layer_info = LayerInfo()
            info.tagged_blocks = TaggedBlocks()
            block = LayerInfoBlock(layer_count=layer_info.layer_count, layer_records=layer_info.layer_records,
                                   channel_image_data=layer_info.channel_image_data)
            info.tagged_blocks[Tag.LAYER_16] = TaggedBlock(key=Tag.LAYER_16,data=block)
        doc.save(ROOT / destination)


if __name__=='__main__':
    generate()
