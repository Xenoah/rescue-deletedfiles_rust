"""Generate independent vector artworks, their compressed documents, and local pixel oracles."""
from pathlib import Path
import argparse
import gzip
import hashlib
import json
import subprocess
import tempfile
from PIL import Image
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--cairo-python", default="/usr/bin/python3")
parser.add_argument("--inkscape", default="inkscape")
parser.add_argument("--rsvg-convert", default="rsvg-convert")
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(exist_ok=False)
svg = "http://www.w3.org/2000/svg"
bodies={
 'rectangle':'<rect x="3" y="4" width="12" height="8" fill="#2a56a0"/>',
 'gradient':'<defs><linearGradient id="g"><stop stop-color="#ef4822"/><stop offset="1" stop-color="#1144bb"/></linearGradient></defs><rect width="30" height="20" fill="url(#g)"/>',
 'clipped':'<defs><clipPath id="c"><circle cx="14" cy="10" r="7"/></clipPath></defs><rect width="28" height="18" fill="#4066e0" clip-path="url(#c)"/>',
 'reused':'<defs><path id="p" d="M1 1L13 2L9 8Z"/></defs><use xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="#p" fill="brown"/><use xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="#p" transform="translate(10 10)" fill="green"/>',
 'stroke':'<g transform="translate(1,2)"><path d="M0 6 Q9 -2 18 8 T28 10" fill="none" stroke="#228844" stroke-width="2"/></g>',
 'unicode':'<title>復元の図形</title><desc>グループ &amp; パス</desc><ellipse cx="17" cy="12" rx="9" ry="5" fill="#a35a80"/>',
}
for i,(name,body) in enumerate(bodies.items()):
 text=f'<svg xmlns="{svg}" width="32" height="24" viewBox="0 0 32 24">{body}</svg>'
 data=text.encode()
 if i==1:data=b'<?xml version="1.0" encoding="UTF-8"?>\n'+data
 if i==2:data=b'\xef\xbb\xbf'+data
 if i==5:data=data.replace(b'<svg ', b'<v:svg xmlns:v="http://www.w3.org/2000/svg" ').replace(b'</svg>',b'</v:svg>');data=b'<?xml version="1.0"?>\n'+data
 (out/f'{name}.svg').write_bytes(data)
with tempfile.TemporaryDirectory() as tmp:
 t=Path(tmp)
 subprocess.run([args.inkscape,out/'gradient.svg','--export-plain-svg','--export-type=svg',f'--export-filename={t}/inkscape.svg'],check=True,capture_output=True)
 (out/'inkscape.svg').write_bytes((t/'inkscape.svg').read_bytes())
 # Cairo's vector output always includes a trailing line feed.
 script='import cairo,sys; s=cairo.SVGSurface(sys.argv[1],32,24); c=cairo.Context(s); c.set_source_rgb(.2,.5,.7); c.arc(13,13,8,0,6.28); c.fill(); s.finish()'
 subprocess.run([args.cairo_python,'-c',script,t/'cairo.svg'],check=True)
 (out/'cairo.svg').write_bytes((t/'cairo.svg').read_bytes())
rsvg=args.rsvg_convert
for p in sorted(out.glob('*.svg')):
 d=p.read_bytes();(out/(p.stem+'.svgz')).write_bytes(gzip.compress(d,compresslevel=6,mtime=0))
manifest={}
with tempfile.TemporaryDirectory() as tmp:
 for p in sorted(out.glob('*.svg*')):
  disk=p.read_bytes();png=Path(tmp)/'vector.png'
  subprocess.run([rsvg,'--output',png,p],check=True)
  with Image.open(png) as im: pixels=im.convert('RGBA').tobytes();sz=im.size
  assert sz==(32,24) and any(pixels) and len(pixels)==3072
  manifest[p.name]=dict(bytes=len(disk),sha256=hashlib.sha256(disk).hexdigest(),rgba_sha256=hashlib.sha256(pixels).hexdigest())
(out/'manifest.json').write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n')
print(len(manifest),'SVG/SVGZ originals and whole-file SHA-256 plus independently rendered RGBA recorded')
