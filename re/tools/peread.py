"""peread.py PE VA [VA...] : print the UTF-16LE (and ASCII) string at each virtual address."""
import struct, sys
b = open(sys.argv[1], 'rb').read()
pe = struct.unpack_from('<I', b, 0x3c)[0]
nsec = struct.unpack_from('<H', b, pe + 6)[0]; opt = struct.unpack_from('<H', b, pe + 20)[0]
base = struct.unpack_from('<Q', b, pe + 24 + 24)[0]
secs = []
for i in range(nsec):
    o = pe + 24 + opt + 40 * i
    vs, va, rs, ro = struct.unpack_from('<IIII', b, o + 8)
    secs.append((va, max(vs, rs), ro))
def off(v):
    r = v - base
    for va, sz, ro in secs:
        if va <= r < va + sz: return ro + r - va
for a in sys.argv[2:]:
    o = off(int(a, 16)); w = b[o:o + 256]
    s16 = w.decode('utf-16le', 'replace').split('\x00')[0]
    s8 = w.split(b'\x00')[0].decode('latin1')
    print(f"{a}: u16={s16!r} ascii={s8!r}")
