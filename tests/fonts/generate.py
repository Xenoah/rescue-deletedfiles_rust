"""Regenerate the original geometric test glyphs with fontTools 4.61.1."""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.sfnt import WOFFFlavorData
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.pens.t2CharStringPen import T2CharStringPen
import json,hashlib,io
out=Path(__file__).resolve().parent
manifest={}
for i,(kind,count) in enumerate([('ttf',1),('ttf',3),('ttf',5),('otf',1),('otf',3),('otf',5)]):
    fb=FontBuilder(1000,isTTF=kind=='ttf')
    letters=[chr(65+j) for j in range(count)]
    glyphs=['.notdef',*letters]
    fb.setupGlyphOrder(glyphs)
    fb.setupCharacterMap({ord(g):g for g in letters})
    shapes={}
    for j,g in enumerate(glyphs):
        pen=TTGlyphPen(None) if kind=='ttf' else T2CharStringPen(600,None)
        if j:
            pen.moveTo((40,0));pen.lineTo((250+20*j,700));pen.lineTo((535,0));pen.closePath()
        shapes[g]=pen.glyph() if kind=='ttf' else pen.getCharString()
    family='Rescue Fixture '+str(i+1); ps='RescueFixture'+str(i+1)
    if kind=='ttf':fb.setupGlyf(shapes)
    else:fb.setupCFF(ps,{'FullName':family,'FamilyName':family,'Weight':'Regular'},shapes,{})
    fb.setupHorizontalMetrics({g:(600,40) for g in glyphs})
    fb.setupHorizontalHeader(ascent=800,descent=-200)
    fb.setupNameTable({'familyName':family,'styleName':'Regular','psName':ps,'fullName':family,'uniqueFontIdentifier':ps+'-1.0'})
    fb.setupOS2(sTypoAscender=800,sTypoDescender=-200,usWinAscent=800,usWinDescent=200)
    fb.setupPost();fb.setupMaxp()
    fb.font['head'].created=fb.font['head'].modified=3520000000;fb.font.recalcTimestamp=False
    if count==5:
        table=newTable('TEST');table.data=b'Independent private table? '*9;fb.font['TEST']=table
    p=out/f'{kind}-{count}.{kind}';fb.font.save(p)
    def record(p):
        data=p.read_bytes();font=TTFont(io.BytesIO(data),checkChecksums=2)
        manifest[p.name]=dict(sha256=hashlib.sha256(data).hexdigest(),family=font['name'].getDebugName(1),characters=sorted(font.getBestCmap()))
    record(p)
    if count in (3,5):
        fb.font.flavor='woff'
        fd=WOFFFlavorData()
        if count==5:
            fd.metaData=b'<metadata version="1.0"><description><text>Rescue fixture</text></description></metadata>'
            fd.privData=b'Independent private data'
        fb.font.flavorData=fd
        w=p.with_suffix('.woff');fb.font.save(w);record(w)
(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
