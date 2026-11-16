# python3 ico.py <dir>: rcad.ico from rcad-{16..256}.png (PNG-compressed entries, Vista+)
import struct,sys,os
d=sys.argv[1]; sizes=[16,24,32,48,64,128,256]
data=[open(os.path.join(d,f'rcad-{s}.png'),'rb').read() for s in sizes]
hdr=struct.pack('<HHH',0,1,len(sizes)); off=6+16*len(sizes); ent=b''
for s,b in zip(sizes,data):
    ent+=struct.pack('<BBBBHHII',s%256,s%256,0,0,1,32,len(b),off); off+=len(b)
open(os.path.join(d,'rcad.ico'),'wb').write(hdr+ent+b''.join(data))
