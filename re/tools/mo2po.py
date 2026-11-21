import struct,sys
d=open(sys.argv[1],'rb').read()
magic=struct.unpack('<I',d[:4])[0]; e='<' if magic==0x950412de else '>'
_,n,oo,to=struct.unpack(e+'4I',d[4:20])
out=[]
def esc(s): return s.replace('\\','\\\\').replace('"','\\"').replace('\n','\\n').replace('\t','\\t')
for i in range(n):
    l,o=struct.unpack(e+'2I',d[oo+8*i:oo+8*i+8]); k=d[o:o+l].decode('utf-8','replace')
    l,o=struct.unpack(e+'2I',d[to+8*i:to+8*i+8]); v=d[o:o+l].decode('utf-8','replace')
    if '\x04' in k: ctx,k=k.split('\x04',1); out.append('msgctxt "%s"'%esc(ctx))
    ks=k.split('\x00'); vs=v.split('\x00')
    out.append('msgid "%s"'%esc(ks[0]))
    if len(ks)>1:
        out.append('msgid_plural "%s"'%esc(ks[1]))
        for j,x in enumerate(vs): out.append('msgstr[%d] "%s"'%(j,esc(x)))
    else: out.append('msgstr "%s"'%esc(v))
    out.append('')
sys.stdout.write('\n'.join(out))
