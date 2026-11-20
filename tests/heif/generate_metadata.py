"""Build five camera metadata fixtures from JPEG APP1 data with Pillow and libheif."""
from pathlib import Path
import base64
import hashlib
import json
import subprocess
import tempfile
from PIL import Image
from generate import png

out = Path(__file__).resolve().parent/'metadata'
out.mkdir(exist_ok=True)
patterns = [('exif.heic', True, False, 1), ('xmp.heic', False, True, 1),
            ('portrait.heic', True, True, 6), ('tagged.avif', True, True, 3),
            ('caption.avif', False, True, 1)]
manifest = {}
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for seed,(name,has_exif,has_xmp,orientation) in enumerate(patterns,1):
        png(root/'source.png',79+seed,61+seed,seed+20)
        options, blocks = {}, {}
        if has_exif:
            exif = Image.Exif()
            exif[0x010f],exif[0x0110],exif[0x0112]='Rescue QA','Test camera '+str(seed),orientation
            options['exif']=exif
        if has_xmp:
            options['xmp']=('<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
                            '<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:description="写真 '+str(seed)+'"/>'
                            '</rdf:RDF></x:xmpmeta>').encode()
        with Image.open(root/'source.png') as image:
            image.save(root/'source.jpg',quality=88,**options)
        with Image.open(root/'source.jpg') as saved:
            if has_exif:
                raw=saved.info['exif']
                assert raw.startswith(b'Exif\0\0')
                blocks['Exif']=raw[6:]
            if has_xmp:
                blocks['mime']=saved.info['xmp']
        codec = ['-A'] if name.endswith('.avif') else ['-p','x265:pools=none','-p','x265:frame-threads=1']
        subprocess.run(['heif-enc',*codec,'-q','71',root/'source.jpg','-o',out/name],check=True)
        data=(out/name).read_bytes()
        assert all(data.count(b)==1 for b in blocks.values())
        manifest[name] = {'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),
                          'jpeg_metadata':{kind:base64.b64encode(b).decode() for kind,b in blocks.items()}}
(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
