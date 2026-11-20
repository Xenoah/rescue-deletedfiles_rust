"""Create independent HEIC originals with Apple's ImageIO encoder via sips."""
from pathlib import Path
import hashlib
import json
import subprocess
import tempfile
from generate import png

out = Path('apple-heic')
out.mkdir()
report = {}
patterns = [('landscape', 176, 98, 3, 8), ('portrait', 67, 123, 3, 8),
            ('alpha', 96, 80, 4, 8), ('gray', 84, 64, 1, 8), ('deep', 112, 72, 3, 16)]
with tempfile.TemporaryDirectory() as tmp:
    for seed, (name, width, height, channels, bits) in enumerate(patterns, 1):
        source, original, rendered = Path(tmp)/'source.png', out/('apple-'+name+'.heic'), Path(tmp)/'decoded.png'
        png(source, width, height, seed+10, channels, bits)
        subprocess.run(['sips','-s','format','heic','-s','formatOptions','75',source,'--out',original],check=True)
        subprocess.run(['sips','-s','format','png',original,'--out',rendered],check=True)
        assert rendered.read_bytes()[:8] == b'\x89PNG\r\n\x1a\n'
        data = original.read_bytes()
        assert data[4:8] == b'ftyp' and data[8:12] in (b'heic', b'heix', b'mif1')
        report[original.name] = dict(bytes=len(data),sha256=hashlib.sha256(data).hexdigest(),encoder='Apple ImageIO (sips)')
(out/'manifest.json').write_text(json.dumps(report,indent=2)+'\n')
