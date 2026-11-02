"""Self-collision and table checks on the CAD meshes (python-fcl BVH, exact triangle tests)."""
import numpy as np, fcl, pickle, itertools, os
import arm
HERE = os.path.dirname(os.path.abspath(__file__))
M = pickle.load(open(HERE + '/model.pkl', 'rb'))
BODIES = ['base', 'J1', 'J2', 'J3', 'J4', 'J5', 'J6', 'tool']
BID = {b: arm.model.body(b).id for b in BODIES}
def bvh(parts):
    v = np.vstack([p['v'] for p in parts]) / 1000.0
    off = np.cumsum([0] + [len(p['v']) for p in parts[:-1]])
    f = np.vstack([p['f'] + o for p, o in zip(parts, off)]).astype(np.int32)
    g = fcl.BVHModel(); g.beginModel(len(v), len(f)); g.addSubModel(v, f); g.endModel()
    return fcl.CollisionObject(g, fcl.Transform()), v

# every body, whole; and for each joint, both sides of it without that joint's actuator, which
# links them by design (housing on one side, carrier on the other), so the links themselves can be checked
objs = {}; verts = {}
for b in BODIES:
    objs[b], v = bvh([p for p in M['coll'] if p['body'] == b]); verts[b] = v[::5]
ADJ = [('base', 'J1'), ('J1', 'J2'), ('J2', 'J3'), ('J3', 'J4'), ('J4', 'J5'), ('J5', 'J6'), ('J6', 'tool')]
for a, b in ADJ:
    j = b if b != 'tool' else None
    for x in (a, b):
        keep = [p for p in M['coll'] if p['body'] == x and not (j and p['part'].startswith(j + ' '))]
        if b == 'tool':   # the coupling: skip the tool changer halves
            keep = [p for p in keep if p['part'] not in ('Tool changer (arm side)', 'V-groove dowels 4x12', 'Magnets 10x3 (arm)', 'Pogo connector 8-pin',
                                                         'Tool plate', 'Coupling balls 8', 'Magnets 10x3 (tool)', 'Tool pad PCB', 'J6 XS20P carrier', 'J6 XS20P output pins')]
        objs[(x, a, b)] = bvh(keep)[0] if keep else None
KEY = {}

def place(q):
    d = arm.fk(q)
    for k, o in objs.items():
        if o is None: continue
        b = k if isinstance(k, str) else k[0]
        R = d.xmat[BID[b]].reshape(3, 3); t = d.xpos[BID[b]]
        o.setTransform(fcl.Transform(R.copy(), t.copy()))
    return d

def hits(q, pairs):
    place(q)
    out = []
    for a, b in pairs:
        oa, ob = (objs[(a, a, b)], objs[(b, a, b)]) if (a, b) in ADJ else (objs[a], objs[b])
        if oa is None or ob is None: continue
        if fcl.collide(oa, ob, fcl.CollisionRequest(), fcl.CollisionResult()):
            out.append((a, b))
    return out

def below_table(q, clearance=0.0):
    """moving bodies (J2 on) that dip below the table top z = 0"""
    d = arm.fk(q); out = []
    for b in ['J2', 'J3', 'J4', 'J5', 'J6', 'tool']:
        R = d.xmat[BID[b]].reshape(3, 3); t = d.xpos[BID[b]]
        if (verts[b] @ R.T + t)[:, 2].min() < clearance: out.append(b)
    return out

ALL = list(itertools.combinations(BODIES, 2))
# pairs already touching in the zero pose are bolted or by design (none expected)
TOUCH0 = hits(np.zeros(6), ALL)
PAIRS = [p for p in ALL if p not in TOUCH0]

def clear(q):
    return not hits(q, PAIRS) and not below_table(q)

def distance(q, a, b):
    place(q)
    if (a, b) in ADJ: oa, ob = objs[(a, a, b)], objs[(b, a, b)]
    else: oa, ob = objs[a], objs[b]
    r = fcl.DistanceResult()
    return fcl.distance(oa, ob, fcl.DistanceRequest(), r)

if __name__ == '__main__':
    import time
    print('touching at zero:', TOUCH0)
    print('checked pairs:', len(PAIRS))
    t = time.time(); n = 200
    rng = np.random.default_rng(0)
    for _ in range(n): hits(rng.uniform(-np.pi, np.pi, 6), PAIRS)
    print('ms per config', (time.time() - t) / n * 1000)
