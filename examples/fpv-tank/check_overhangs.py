# Overhang report for STLs lying in print orientation (bed at z=min).
# Faces whose normal points down more steeply than 45° from vertical and that are not on the bed
# are grouped into connected regions; each region is printed with its area, bbox and the steepest angle.
import sys, struct, numpy as np
def load(p):
    b = open(p,'rb').read()
    if b[:5] == b'solid' and b'facet' in b[:300]:
        v = [list(map(float, l.split()[1:])) for l in b.decode().splitlines() if l.strip().startswith('vertex')]
        return np.array(v).reshape(-1,3,3)
    n = struct.unpack('<I', b[80:84])[0]
    a = np.frombuffer(b[84:84+n*50], dtype=np.dtype([('n','<3f4'),('v','<9f4'),('a','<u2')]))
    return a['v'].reshape(-1,3,3).astype(float)
for p in sys.argv[1:]:
    t = load(p); z0 = t[:,:,2].min()
    nrm = np.cross(t[:,1]-t[:,0], t[:,2]-t[:,0]); ar = np.linalg.norm(nrm,axis=1)/2
    ok = ar > 1e-9; nz = np.zeros(len(t)); nz[ok] = nrm[ok,2]/(2*ar[ok])
    bad = (nz < -0.7072) & (t[:,:,2].min(1) > z0 + 0.25)
    idx = np.where(bad)[0]
    # union-find over shared vertices
    key = {}; par = list(range(len(idx)))
    def f(i):
        while par[i]!=i: par[i]=par[par[i]]; i=par[i]
        return i
    for k,i in enumerate(idx):
        for v in t[i]:
            kk = tuple(np.round(v,3))
            if kk in key: par[f(k)] = f(key[kk])
            else: key[kk]=k
    groups = {}
    for k in range(len(idx)): groups.setdefault(f(k),[]).append(idx[k])
    rs = []
    for g in groups.values():
        a = ar[g].sum()
        if a < 0.5: continue
        pts = t[g].reshape(-1,3); lo=pts.min(0); hi=pts.max(0)
        ang = np.degrees(np.arccos(np.clip(-nz[g].min(),-1,1)))
        rs.append((a,lo,hi,np.degrees(np.arccos(-nz[g])).min()))
    rs.sort(key=lambda r:-r[0])
    print(f"== {p.split('/')[-1]}: {len(rs)} overhang regions, {sum(r[0] for r in rs):.0f} mm²")
    for a,lo,hi,ang in rs[:25]:
        s = hi-lo
        print(f"   {a:7.1f} mm²  size {s[0]:5.1f}×{s[1]:5.1f}  z {lo[2]-z0:5.1f}..{hi[2]-z0:5.1f}  at x{(lo[0]+hi[0])/2:6.1f} y{(lo[1]+hi[1])/2:6.1f}  min tilt from flat {ang:4.0f}°")
