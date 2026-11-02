"""Build the rigid-body model of the 6-axis arm from rcad's per-part meshes."""
import json, numpy as np, trimesh, pickle, os
D = os.path.join(os.environ.get('SIMOUT', 'sim-meshes'), '')   # folder written by `rcad script parts.rhai <folder> sim`
OUT = os.path.dirname(os.path.abspath(__file__))
sp = json.load(open(D + 'sim-parts.json'))
G = sp; H2, L1, W, yF, xT = G['H2'], G['L1'], G['W'], G['yF'], G['xT']
BODIES = ['base', 'J1', 'J2', 'J3', 'J4', 'J5', 'J6', 'tool']
# joint axes in the zero pose (world, mm): point, direction -- the same as parts.rhai `axes`
AXES = {'J1': ([0, 0, 0], [0, 0, 1]), 'J2': ([0, 0, H2], [0, 1, 0]), 'J3': ([L1, 0, H2], [0, 1, 0]),
        'J4': ([0, yF, H2], [1, 0, 0]), 'J5': ([W, 0, H2], [0, 1, 0]), 'J6': ([0, yF, H2], [1, 0, 0])}
PAYLOAD = 1.0  # kg, tool included
TOOL_CG = np.array([xT + 45.0, yF, H2])

def mass_props(mesh, mass_kg):
    """mass, com (m), inertia about com (kg m^2) for a mesh scaled to the given mass"""
    m = mesh.copy(); m.apply_scale(1e-3)
    vol = abs(m.volume)
    if vol < 1e-12:
        return mass_kg, m.centroid, np.zeros((3, 3))
    m.density = mass_kg / vol
    if m.volume < 0: m.invert()
    I = m.moment_inertia  # about com, world axes
    return mass_kg, m.center_mass, I

def combine(items):
    M = sum(i[0] for i in items)
    c = sum(i[0] * np.asarray(i[1]) for i in items) / M
    I = np.zeros((3, 3))
    for m, ci, Ii in items:
        d = np.asarray(ci) - c
        I += Ii + m * (np.dot(d, d) * np.eye(3) - np.outer(d, d))
    return M, c, I

def build():
    body_items = {b: [] for b in BODIES}
    body_meshes = {b: [] for b in BODIES}
    web = []
    for p in sp['parts']:
        mesh = trimesh.load(D + p['file'])
        mesh.merge_vertices()
        b = p['group']
        if p['mass_g'] is not None:
            body_items[b].append(mass_props(mesh, p['mass_g'] / 1000))
        body_meshes[b].append((p['part'], mesh))
        # light mesh for the viewer
        nf = len(mesh.faces)
        target = int(min(nf, max(300, nf * 0.12)))
        lm = mesh.simplify_quadric_decimation(face_count=target) if target < nf else mesh
        web.append(dict(part=p['part'], body=b, color=p['color'], kind=p['kind'], material=p['material'],
                        v=np.asarray(lm.vertices, np.float32), f=np.asarray(lm.faces, np.uint32)))
    # payload: 1 kg (tool included) at rcad's tool centre of gravity, inertia of a 60x50x95 block
    a, bb, c = 0.095, 0.050, 0.060
    Itool = PAYLOAD / 12 * np.diag([bb**2 + c**2, a**2 + c**2, a**2 + bb**2])
    body_items['tool'] = [(PAYLOAD, TOOL_CG / 1000, Itool)]
    inertial = {b: combine(body_items[b]) for b in BODIES}
    # collision meshes: one per body, decimated
    # collision meshes per part, decimated to at most 4000 faces each
    coll = []
    for b in BODIES:
        for name, m in body_meshes[b]:
            nf = len(m.faces)
            mm = m.simplify_quadric_decimation(face_count=4000) if nf > 4000 else m
            coll.append(dict(part=name, body=b, v=np.asarray(mm.vertices), f=np.asarray(mm.faces)))
    pickle.dump(dict(inertial=inertial, coll=coll, web=web),
                open(OUT + '/model.pkl', 'wb'))
    return inertial

def mjcf(inertial, armature=None):
    """MuJoCo model: all body frames coincide with the world frame in the zero pose; joints at their axes."""
    def inert(b):
        M, c, I = inertial[b]
        return (f'<inertial pos="{c[0]:.6f} {c[1]:.6f} {c[2]:.6f}" mass="{M:.6f}" '
                f'fullinertia="{I[0,0]:.3e} {I[1,1]:.3e} {I[2,2]:.3e} {I[0,1]:.3e} {I[0,2]:.3e} {I[1,2]:.3e}"/>')
    x = ['<mujoco model="robot-arm-6dof"><compiler angle="radian"/><option gravity="0 0 -9.81" timestep="0.001"/><worldbody>']
    x.append(f'<body name="base">{inert("base")}')
    for j in ['J1', 'J2', 'J3', 'J4', 'J5', 'J6']:
        p, a = AXES[j]
        arm = '' if armature is None else f' armature="{armature[j]:.5f}"'
        x.append(f'<body name="{j}">{inert(j)}<joint name="{j}" type="hinge" pos="{p[0]/1000} {p[1]/1000} {p[2]/1000}" axis="{a[0]} {a[1]} {a[2]}"{arm}/>')
    x.append(f'<body name="tool">{inert("tool")}<site name="tcp" pos="{(xT+81)/1000} {yF/1000} {H2/1000}"/>'
             f'<site name="iface" pos="{xT/1000} {yF/1000} {H2/1000}"/></body>')
    x.append('</body>' * 6 + '</body></worldbody></mujoco>')
    return ''.join(x)

if __name__ == '__main__':
    inertial = build()
    for b in BODIES:
        M, c, I = inertial[b]
        print(b, f'{M*1000:7.0f} g  com {np.round(c*1000,1)}  Idiag {np.round(np.diag(I)*1e4,1)} kg cm2')
    open(OUT + '/arm.xml', 'w').write(mjcf(inertial))
