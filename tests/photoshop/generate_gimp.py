"""Run inside GIMP 3's python-fu-eval interpreter with RESCUE_PSD_FIXTURES set."""
import os
from pathlib import Path
from gi.repository import Gio

folder = Path(os.environ['RESCUE_PSD_FIXTURES'])
source = folder.parent/'corpus/files/sample.bmp'
for label in ('layers','mask','gray','high-depth','group'):
    doc = Gimp.file_load(Gimp.RunMode.NONINTERACTIVE,Gio.File.new_for_path(str(source)))
    base = doc.get_layers()[0]
    base.set_name('Original')
    layer = base.copy()
    layer.set_name('編集 layer '+label)
    group = None
    if label == 'group':
        group = Gimp.GroupLayer.new(doc,'Color group')
        doc.insert_layer(group,None,0)
    doc.insert_layer(layer,group,0)
    layer.add_alpha()
    layer.set_offsets(4,3)
    layer.set_opacity(55.0)
    if label == 'mask':
        mask = layer.create_mask(Gimp.AddMaskType.WHITE)
        assert layer.add_mask(mask)
    if label == 'gray':
        assert doc.convert_grayscale()
    if label == 'high-depth':
        assert doc.convert_precision(Gimp.Precision.U16_NON_LINEAR)
    target = folder/('gimp-'+label+'.psd')
    assert Gimp.file_save(Gimp.RunMode.NONINTERACTIVE,doc,Gio.File.new_for_path(str(target))),label
    check = Gimp.file_load(Gimp.RunMode.NONINTERACTIVE,Gio.File.new_for_path(str(target)))
    print(label,check.get_width(),check.get_height(),len(check.get_layers()),flush=True)
    check.delete()
    doc.delete()
