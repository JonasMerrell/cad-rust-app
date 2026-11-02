"""Run the simulations and write results.json (for the viewer) and plots."""
import numpy as np, json, os, time
import arm, collide
from arm import JOINTS
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.environ.get('OUT', HERE + '/out'); os.makedirs(OUT, exist_ok=True)
deg = np.degrees; rad = np.radians
R = {}

# ================================================================ tool changer holding (kinematic coupling + magnets)
# Geometry from parts.rhai, tool frame: x = coupling axis (out of the arm), origin at the arm-side face centre.
# Three Ø8 balls at r 24 (90°, 210°, 330°), each between two Ø4 dowels whose axes run radially at x = -0.5,
# 2.6 mm either side of the groove centre; the ball seats where it touches both (centre x = 4.91).
# Three magnet pairs at r 22 (30°, 150°, 270°) pull the tool in with F_MAG each.
F_MAG = 20.0   # N per magnet pair (3 x Ø10x3 N52, about 60 N together, as the README assumes)
RB, RM, RBALL, RPIN, PIN_X, PIN_T = 0.024, 0.022, 0.004, 0.002, -0.0005, 0.0026
BALL_X = PIN_X + np.sqrt((RBALL + RPIN) ** 2 - PIN_T ** 2)
CONTACT_ANGLE = np.degrees(np.arctan2(PIN_T, BALL_X - PIN_X))   # contact normal from the coupling axis
cols = []
for k in range(3):
    a = np.radians(90 + 120 * k)
    u = np.array([0, np.cos(a), np.sin(a)]); v = np.array([0, -np.sin(a), np.cos(a)])
    b = np.array([BALL_X, 0, 0]) + RB * u
    for sgn in (-1, 1):
        pin = np.array([PIN_X, 0, 0]) + RB * u + sgn * PIN_T * v
        n = (b - pin) / np.linalg.norm(b - pin)       # force on the ball (tool) from the pin
        p = b - RBALL * n
        cols.append(np.concatenate([n, np.cross(p, n)]))
A6 = np.array(cols).T                                   # wrench on the tool per unit contact force
mag_w = np.zeros(6)
for k in range(3):
    m = np.radians(30 + 120 * k); p = np.array([0, RM * np.cos(m), RM * np.sin(m)]); f = np.array([-F_MAG, 0, 0])
    mag_w += np.concatenate([f, np.cross(p, f)])
c_mag = np.linalg.solve(A6, -mag_w)                     # contact forces from the magnets alone (all > 0)

def coupling_sf(F, M, Rt):
    """factor by which the load could grow before a ball starts to lift off its dowels"""
    load = np.concatenate([Rt.T @ F, Rt.T @ M])         # wrench the arm puts into the tool, tool frame
    c_load = np.linalg.solve(A6, load)
    neg = c_load < -1e-12
    return float(np.min(-c_mag[neg] / c_load[neg])) if neg.any() else np.inf

# A[1]: My from a +x ball force at (y, z) = (r cos a, r sin a) is z*f = r sin a * f; A[2]: Mz = -y*f
q_flat = np.zeros(6)
F, M, Rt = arm.tool_wrench(q_flat, np.zeros(6), np.zeros(6))
R['coupling_static_flat'] = coupling_sf(F, M, Rt); R['coupling_contact_angle_deg'] = CONTACT_ANGLE; R['coupling_ball_x_mm'] = BALL_X * 1000
# worst static orientation: try tool roll angles while flat
R['coupling_static_worst'] = min(coupling_sf(*arm.tool_wrench(np.array([0, 0, 0, 0, 0, a]), np.zeros(6), np.zeros(6))) for a in rad(np.arange(0, 360, 5)))
print('coupling SF static flat', R['coupling_static_flat'], 'worst roll', R['coupling_static_worst'])

# ================================================================ statics: torque need over the arm's plane, payload capacity
def set_payload(m):
    tb = arm.TOOL
    arm.model.body_mass[tb] = m
    arm.model.body_inertia[tb] = BASE_I * m
BASE_I = arm.model.body_inertia[arm.TOOL].copy()
usable0 = np.array([arm.usable_joint_torque(j) for j in JOINTS])
R['usable_static_Nm'] = dict(zip(JOINTS, usable0.round(2).tolist()))
g2 = np.arange(-120, 41, 4); g3 = np.arange(-150, 61, 4)
need = np.full((len(g3), len(g2), 6), np.nan); cap = np.full((len(g3), len(g2)), np.nan)
for a, j3 in enumerate(g3):
    for b, j2 in enumerate(g2):
        q = rad(np.array([0, j2, j3, 0, -(j2 + j3), 0]))   # tool axis kept horizontal: worst for J5
        if not collide.clear(q): continue
        set_payload(1.0); t1 = np.abs(arm.inverse(q, np.zeros(6), np.zeros(6)))
        set_payload(0.0); t0 = np.abs(arm.inverse(q, np.zeros(6), np.zeros(6)))
        need[a, b] = t1 / usable0
        per_kg = t1 - t0
        with np.errstate(divide='ignore'):
            cap[a, b] = np.min(np.where(per_kg[1:5] > 1e-6, (usable0[1:5] - t0[1:5]) / per_kg[1:5], np.inf))
set_payload(1.0)
R['static_map'] = dict(j2=g2.tolist(), j3=g3.tolist(),
                       ratio=[[None if np.isnan(need[a, b, 1]) else round(float(np.nanmax(need[a, b, 1:5])), 3) for b in range(len(g2))] for a in range(len(g3))],
                       payload=[[None if np.isnan(cap[a, b]) else round(float(cap[a, b]), 2) for b in range(len(g2))] for a in range(len(g3))])
R['payload_capacity_min_kg'] = float(np.nanmin(cap))
R['static_ratio_max'] = float(np.nanmax(need[:, :, 1:5]))
i = np.unravel_index(np.nanargmax(cap * 0 - need[:, :, 1:5].max(axis=2) * -1), cap.shape)
print('static: max need/usable', R['static_ratio_max'], 'min payload capacity kg', R['payload_capacity_min_kg'])

# ================================================================ collision map J2 x J3 and J5 range
t0_ = time.time()
c2 = np.arange(-180, 181, 2); c3 = np.arange(-180, 181, 2)
cmap = np.zeros((len(c3), len(c2)), int)   # 0 free, 1 table, 2 self
for a, j3 in enumerate(c3):
    for b, j2 in enumerate(c2):
        q = rad(np.array([0, j2, j3, 0, 0, 0]))
        if collide.hits(q, collide.PAIRS): cmap[a, b] = 2
        elif collide.below_table(q): cmap[a, b] = 1
R['collision_map'] = dict(j2=c2.tolist(), j3=c3.tolist(), state=cmap.tolist())
# self-collision limits of J3 (independent of J2) and J5 (against J4 roll)
def limit(jidx, base, sign, stop=180):
    for a in range(0, stop + 1):
        q = np.array(base, float); q[jidx] = rad(sign * a)
        h = collide.hits(q, collide.PAIRS)
        if h: return sign * (a - 1), h
    return sign * stop, []
R['J3_self_limits'] = [limit(2, [0, rad(-90), 0, 0, 0, 0], s)[0] for s in (-1, 1)]
j5 = {}
for r4 in range(0, 181, 15):
    j5[r4] = [limit(4, [0, rad(-60), rad(60), rad(r4), 0, 0], s) for s in (-1, 1)]
R['J5_self_limits_by_J4'] = {k: [v[0][0], v[1][0]] for k, v in j5.items()}
print('J3 self limits', R['J3_self_limits'], 'J5', R['J5_self_limits_by_J4'], 'collision maps', round(time.time() - t0_), 's')

# recommended soft limits (a couple of degrees inside the hard stops found above; J1/J4/J6 for cables)
LIM = {'J1': (-170, 170), 'J2': (-180, 20), 'J3': (R['J3_self_limits'][0] + 3, R['J3_self_limits'][1] - 3),
       'J4': (-170, 170), 'J5': (max(v[0] for v in R['J5_self_limits_by_J4'].values()) + 3, min(v[1] for v in R['J5_self_limits_by_J4'].values()) - 3), 'J6': (-170, 170)}
R['limits_deg'] = LIM

# ================================================================ workspace (Monte Carlo inside the limits)
rng = np.random.default_rng(1)
lo = np.array([LIM[j][0] for j in JOINTS]); hi = np.array([LIM[j][1] for j in JOINTS])
pts = []; nfree = 0; N = 40000; pair_count = {}
for _ in range(N):
    qd_ = rng.uniform(lo, hi); q = rad(qd_)
    h = collide.hits(q, collide.PAIRS); tb = collide.below_table(q)
    for p in h: pair_count['/'.join(p)] = pair_count.get('/'.join(p), 0) + 1
    if h or tb: continue
    nfree += 1
    d = arm.fk(q)
    w = d.site_xpos[arm.IFACE].copy()
    F, M, Rt = arm.tool_wrench(q, np.zeros(6), np.zeros(6))
    t = np.abs(arm.inverse(q, np.zeros(6), np.zeros(6))) / usable0
    pts.append([float(np.hypot(w[0], w[1])), float(w[2]), float(t[1:].max())])
pts = np.array(pts)
R['workspace'] = dict(samples=N, free_fraction=nfree / N, collisions=pair_count,
                      max_reach_from_J1_mm=float(pts[:, 0].max() * 1000),
                      max_height_mm=float(pts[:, 1].max() * 1000), pts=np.round(pts[::4] * [1000, 1000, 1], 1).tolist())
print('workspace free', nfree / N, 'reach', pts[:, 0].max(), pair_count)

# ================================================================ pick-and-place cycle, timed as fast as the steppers allow
DOWN = np.array([0, 0, -1.0])
def ikd(p, seed):
    q, e = arm.ik(np.array(p) / 1000, DOWN, seed, active=(0, 1, 2, 4))
    assert e < 1e-4, (p, e)
    return q
HOME = rad(np.array([0, -90, 90, 0, 0, 0]))     # upper arm vertical, forearm forward, tool out
seed = rad(np.array([0, -45, 80, 0, 55, 0]))
PICK = (330, -200, 60); PLACE = (250, 300, 60); LIFT = 160
qa_up = ikd((PICK[0], PICK[1], PICK[2] + LIFT), seed); qa = ikd(PICK, qa_up)
qb_up = ikd((PLACE[0], PLACE[1], PLACE[2] + LIFT), seed); qb = ikd(PLACE, qb_up)
qb_up[5] = qb[5] = rad(90)     # turn the part 90° on the way

from scipy.interpolate import CubicSpline
def line(p0, p1, q_seed, j6):
    """IK along a straight line; returns q(s), dq/ds, d2q/ds2 for s in [0, 1] (cubic spline through 41 IK points)"""
    ss = np.linspace(0, 1, 41); qs = []; q = q_seed.copy()
    for s in ss:
        q = ikd(tuple(np.array(p0) + s * (np.array(p1) - np.array(p0))), q); q[5] = j6; qs.append(q.copy())
    cs = CubicSpline(ss, np.array(qs), axis=0)
    return lambda s: (cs(s), cs(s, 1), cs(s, 2))

SEGS = [('Move to the pick', 'joint', HOME, qa_up), ('Down to the part', 'line', (PICK[0], PICK[1], PICK[2] + LIFT), PICK, qa_up),
        ('Grip', 'dwell', 0.4), ('Lift', 'line', PICK, (PICK[0], PICK[1], PICK[2] + LIFT), qa),
        ('Carry across', 'joint', qa_up, qb_up), ('Down to the place', 'line', (PLACE[0], PLACE[1], PLACE[2] + LIFT), PLACE, qb_up),
        ('Release', 'dwell', 0.4), ('Lift', 'line', PLACE, (PLACE[0], PLACE[1], PLACE[2] + LIFT), qb), ('Home', 'joint', qb_up, HOME)]

def mj(tau):   # minimum-jerk profile and its derivatives
    return 10 * tau**3 - 15 * tau**4 + 6 * tau**5, (30 * tau**2 - 60 * tau**3 + 30 * tau**4), (60 * tau - 180 * tau**2 + 120 * tau**3)

def seg_path(seg):
    if seg[1] == 'joint':
        q0, q1 = seg[2], seg[3]
        return lambda s: (q0 + np.outer(s, q1 - q0), np.tile(q1 - q0, (len(s), 1)), np.zeros((len(s), 6)))
    return line(seg[2], seg[3], seg[4], seg[4][5])

def evaluate(path, T, n=120):
    tau = np.linspace(0, 1, n); s, sd, sdd = mj(tau); sd = sd / T; sdd = sdd / T**2
    q, dq, ddq = path(s)
    qd = dq * sd[:, None]; qdd = ddq * sd[:, None]**2 + dq * sdd[:, None]
    return tau * T, q, qd, qdd

def ratios(q, qd, qdd):
    out = np.zeros((len(q), 6)); wm_all = np.zeros((len(q), 6)); tm_all = np.zeros((len(q), 6)); tau_all = np.zeros((len(q), 6))
    for i in range(len(q)):
        wm, tm, tau = arm.motor_demand(q[i], qd[i], qdd[i])
        avail = np.array([arm.SAFETY * arm.pullout(j, wm[k]) for k, j in enumerate(JOINTS)])
        out[i] = tm / np.maximum(avail, 1e-9); wm_all[i] = wm; tm_all[i] = tm; tau_all[i] = tau
    return out, wm_all, tm_all, tau_all

timed = []
for seg in SEGS:
    if seg[1] == 'dwell':
        timed.append((seg[0], seg[2], None)); continue
    path = seg_path(seg)
    lo_, hi_ = 0.05, 20.0
    for _ in range(30):
        T = (lo_ * hi_) ** 0.5
        r = ratios(*evaluate(path, T, 100)[1:])[0]
        if r.max() > 1: lo_ = T
        else: hi_ = T
    timed.append((seg[0], hi_, path))
# sample the whole cycle at 50 fps
FPS = 100
frames = dict(t=[], q=[], seg=[])
t_acc = 0; qlast = HOME; seg_info = []
def cycle_arrays(scale):
    """the cycle run `scale` times faster than the stepper-limited timing"""
    out = dict(t=[], q=[], qd=[], qdd=[], seg=[])
    t_acc = 0; qlast = HOME
    for k, (name, T, path) in enumerate(timed):
        Ts = T / scale if path is not None else T
        n = max(2, int(round(Ts * FPS)))
        if path is None:
            tt = np.linspace(0, Ts, n, endpoint=False); q = np.repeat(qlast[None], n, 0); qd = np.zeros_like(q); qdd = np.zeros_like(q)
        else:
            tt, q, qd, qdd = evaluate(path, Ts, n + 1); tt, q, qd, qdd = tt[:-1], q[:-1], qd[:-1], qdd[:-1]; qlast = path(np.array([1.0]))[0][0]
        out['t'] += (tt + t_acc).tolist(); out['q'].append(q); out['qd'].append(qd); out['qdd'].append(qdd); out['seg'] += [k] * len(tt)
        t_acc += Ts
    for key in ('q', 'qd', 'qdd'): out[key] = np.vstack(out[key])
    out['T'] = t_acc
    return out
base = cycle_arrays(1.0)
# collisions along the path
path_hits = set()
for q in base['q']:
    for p in collide.hits(q, collide.PAIRS): path_hits.add('/'.join(p))
    for b in collide.below_table(q, -0.001): path_hits.add(b + '/table')
min_clear = {}
SCALES = [0.5, 0.75, 1.0, 1.25, 1.5]
runs = {}
for sc in SCALES:
    c = cycle_arrays(sc)
    r, wm, tm, tau = ratios(c['q'], c['qd'], c['qdd'])
    sf = [coupling_sf(*arm.tool_wrench(c['q'][i], c['qd'][i], c['qdd'][i])) for i in range(len(c['q']))]
    tcp_v = []
    for i in range(len(c['q'])):
        p0 = arm.tcp(c['q'][i])[0]; tcp_v.append(p0)
    tcp_v = np.array(tcp_v); v = np.linalg.norm(np.gradient(tcp_v, np.array(c['t']), axis=0), axis=1)
    runs[str(sc)] = dict(T=c['T'], t=np.round(c['t'], 3).tolist(), seg=c['seg'], q=np.round(deg(c['q']), 2).tolist(),
                         ratio=np.round(r, 3).tolist(), rpm=np.round(wm * 60 / (2 * np.pi), 0).tolist(), tau=np.round(tau, 2).tolist(),
                         coupling_sf=np.round(np.minimum(sf, 99), 2).tolist(), tcp_speed=np.round(v * 1000, 0).tolist(),
                         peak_ratio=np.round(r.max(axis=0), 3).tolist(), peak_rpm=np.round(np.abs(wm).max(axis=0) * 60 / (2 * np.pi)).tolist(),
                         peak_tau=np.round(np.abs(tau).max(axis=0), 2).tolist(), min_sf=float(np.min(sf)), max_tcp_mm_s=float(v.max() * 1000))
    print('scale', sc, 'T', round(c['T'], 2), 'peak ratio', np.round(r.max(axis=0), 2), 'rpm', runs[str(sc)]['peak_rpm'], 'SF', round(min(sf), 2), 'v', round(v.max() * 1000))
R['cycle'] = dict(segments=[dict(name=n, T=round(T, 3)) for n, T, _ in timed], runs=runs, path_collisions=sorted(path_hits),
                  pick_mm=PICK, place_mm=PLACE, lift_mm=LIFT)
# which joint limits each segment
lim_by = []
c = cycle_arrays(1.0); r = ratios(c['q'], c['qd'], c['qdd'])[0]
for k in range(len(timed)):
    m = np.array(c['seg']) == k
    lim_by.append(None if timed[k][2] is None else JOINTS[int(np.argmax(r[m].max(axis=0)))])
R['cycle']['limited_by'] = lim_by
# torque-speed curves for the viewer
w = np.linspace(0, 2000, 81) * 2 * np.pi / 60
R['motors'] = {j: dict(name=arm.MOTORS[j]['name'], ratio=arm.RATIO[j], rpm=(w * 60 / 2 / np.pi).round(0).tolist(),
                       pullout=np.round(arm.pullout(j, w), 4).tolist(), drive_A=arm.drive_current(j), holding=arm.MOTORS[j]['T']) for j in JOINTS}
json.dump(R, open(OUT + '/results.json', 'w'))
print('cycle segments', [(n, round(T, 2)) for n, T, _ in timed], 'limited by', lim_by, 'path hits', path_hits)
