from pathlib import Path
import hashlib,random,sqlite3,struct,wave,zipfile
from PIL import Image
r=Path(__file__).resolve().parent/'files';r.mkdir(exist_ok=True)
noise=random.Random(90731).randbytes(222*209*3)
im=Image.frombytes('RGB',(222,209),noise)
for suffix in ['bmp','png','jpg']:im.save(r/('original.'+suffix))
with wave.open(str(r/'original.wav'),'wb') as wav:
 wav.setparams((2,2,44100,0,'NONE','not compressed'));wav.writeframes(noise[:120000])
with zipfile.ZipFile(r/'original.zip','w') as z:
 info=zipfile.ZipInfo('sample.bin',(2020,1,2,3,4,6));z.writestr(info,noise,compress_type=zipfile.ZIP_DEFLATED)
p=r/'original.sqlite'
if not p.exists():
 con=sqlite3.connect(p);con.execute('create table example (id integer primary key, sample blob not null)')
 for i in range(60):con.execute('insert into example(sample) values(?)',(noise[i*2048:(i+1)*2048],))
 con.commit();con.close()
