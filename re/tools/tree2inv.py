"""Turn a winctl tree dump into a readable control inventory for one top-level window."""
import re, sys
src, title, dst = sys.argv[1], sys.argv[2], sys.argv[3]
raw = open(src, encoding='utf-8').read().split('\n')
lines = []
for l in raw:                                   # re-join multi-line window texts
    if re.match(r'^ *([0-9A-F]{16} |- )', l) or not lines: lines.append(l)
    else: lines[-1] += ' / ' + l
rec = re.compile(r'^( *)([0-9A-F]{16}) id=(-?\d+) cls=(\S+) text="(.*?)" rect=(-?\d+),(-?\d+) (\d+)x(\d+) vis=(\d) en=(\d) style=([0-9a-f]+)$')
out, inblk, keep = [], False, False
for l in lines:
    m = rec.match(l)
    if not m:
        if inblk and keep and l.strip().startswith('- '): out.append('    ' + l.strip())
        continue
    ind, h, i, c, t, x, y, w, hh, v, en, st = m.groups()
    if not ind: inblk = (t == title)
    keep = False
    if not inblk: continue
    if c.startswith('wx') and t in ('panel', 'control', 'text', ''): continue
    if c == 'Static' and not t.strip(): continue
    if c == 'Edit' and i == '1001': continue        # combo's inner edit
    keep = True
    flags = ('' if v == '1' else ' [hidden-tab]') + ('' if en == '1' else ' [disabled]')
    out.append(f"{c} id={i} {w}x{hh}: {t!r}{flags}")
open(dst, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(len(out), 'lines')
