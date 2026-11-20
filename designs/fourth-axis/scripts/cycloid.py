#!/usr/bin/env python3
"""Cycloidal reducer for the A axis: geometry, cuttability, and a Monte Carlo tolerance study of lost motion and
transmission error for parts cut on the DMC2.

    cycloid.py            -> variant-cycloidal/cycloid.json, cycloid.md, cycloid-profile.png

Planar rigid-body model. One disc on an eccentric inside a ring of N fixed pins; the output is taken from the disc by pins
in oversize holes. Errors drawn per trial:
  - ring pin holes: each centre within +/- TOL (normal, 2 sigma = TOL), plus a common radius error of the bolt circle;
  - disc profile: a constant offset (cutter radius, deflection) within +/- TOL and a 2-lobe waviness of TOL/3;
  - eccentric throw +/- TOL/3, eccentric bearing radial clearance (CN class, G);
  - output holes in the disc: centre within +/- TOL, radius within +/- TOL/2.
For each trial the free play of the output is found over one output turn (the disc is turned each way until a pin touches),
with and without the two-disc split-carrier preload (two discs, each with its own three output pins on carriers that are
twisted against each other and locked: each disc then drives on one flank only).
"""
import os, json, math, sys
import numpy as np
from scipy.spatial import cKDTree

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "cycloidal")
N, R, RP, E = 24, 44.0, 4.0, 1.25          # ring pins, pin circle radius, pin radius, eccentricity (mm)
RO, RQ, NO = 29.0, 5.0, 3                  # output pin circle radius, output pin radius, pins per disc
TOL = 0.03                                 # what the mill holds on a contour or a hole position (assumption, see REPORT)
C_PROFILE = 0.06                           # designed clearance: the disc is cut this much under size all round so it cannot bind
C_HOLE = 0.06                              # same for the output holes
CB = 0.012                                 # eccentric bearing radial clearance (6004 CN: 5 to 20 um; G)
TOOL_R = 3.175
RATIO = N - 1


def profile(n_pts=6000, offset=0.0, wav=0.0, wav_phase=0.0, e=E):
    """Disc outline (equidistant of the shortened epitrochoid), disc frame, plus an outward offset and waviness."""
    t = np.linspace(0, 2*np.pi, n_pts, endpoint=False)
    psi = np.arctan2(np.sin((1 - N)*t), (R/(e*N)) - np.cos((1 - N)*t))
    x = R*np.cos(t) - RP*np.cos(t + psi) - e*np.cos(N*t)
    y = -R*np.sin(t) + RP*np.sin(t + psi) + e*np.sin(N*t)
    p = np.stack([x, y], 1)
    # outward normal by finite differences
    d = np.roll(p, -1, 0) - np.roll(p, 1, 0); nrm = np.stack([d[:, 1], -d[:, 0]], 1); nrm /= np.linalg.norm(nrm, axis=1)[:, None]
    if np.mean(np.sum(nrm*p, 1)) < 0: nrm = -nrm
    ang = np.arctan2(p[:, 1], p[:, 0])
    return p + nrm*(offset + wav*np.cos(2*ang + wav_phase))[:, None]


def curvature_radius(p):
    d1 = (np.roll(p, -1, 0) - np.roll(p, 1, 0))/2; d2 = np.roll(p, -1, 0) - 2*p + np.roll(p, 1, 0)
    k = (d1[:, 0]*d2[:, 1] - d1[:, 1]*d2[:, 0])/np.linalg.norm(d1, axis=1)**3
    return k            # signed curvature along the parameter direction


class Trial:
    def __init__(self, rng, tol=TOL, c_prof=C_PROFILE, c_hole=C_HOLE, perfect=False):
        z = (lambda *a, **k: 0.0) if perfect else None
        u = (lambda lo, hi, size=None: np.zeros(size) if size else 0.0) if perfect else rng.uniform
        g = (lambda s, size=None: np.zeros(size) if size else 0.0) if perfect else (lambda s, size=None: np.clip(rng.normal(0, s, size), -2*s, 2*s))
        a = 2*np.pi*np.arange(N)/N
        dr = u(-tol*0.67, tol*0.67)
        self.pins = np.stack([(R + dr)*np.cos(a), (R + dr)*np.sin(a)], 1) + g(tol/2, (N, 2))
        self.e = E + u(-tol/3, tol/3)
        self.prof = profile(offset=-c_prof + u(-tol, tol), wav=u(0, tol/3), wav_phase=u(0, 2*np.pi), e=E)
        self.tree = cKDTree(self.prof)
        ao = 2*np.pi*np.arange(NO)/NO
        self.holes = np.stack([RO*np.cos(ao), RO*np.sin(ao)], 1) + g(tol/2, (NO, 2))          # hole centres in the disc
        self.hole_r = RQ + E + c_hole + u(-tol/2, tol/2, NO)
        self.opins = np.stack([RO*np.cos(ao), RO*np.sin(ao)], 1) + g(tol/2, (NO, 2))          # output pins on their carrier
        self.cb = 0.0 if perfect else CB

    def ring_gap(self, theta, psi, shift=0.0):
        """Smallest gap between the disc and the ring pins; disc centre on the eccentric at input angle theta, disc turned psi."""
        c = self.e*np.array([np.cos(theta), np.sin(theta)]) + shift*np.array([-np.sin(theta), np.cos(theta)])
        cs, sn = np.cos(-psi), np.sin(-psi)
        q = self.pins - c; q = np.stack([cs*q[:, 0] - sn*q[:, 1], sn*q[:, 0] + cs*q[:, 1]], 1)
        d, _ = self.tree.query(q)
        return float(np.min(d) - RP)

    def disc_range(self, theta):
        """[psi-, psi+]: how far the disc can turn each way about its nominal angle before a ring pin stops it."""
        psi0 = SIGN*theta/(N - 1)
        # locate the free position near the nominal one
        best, bg = psi0, -1
        for dp in np.linspace(-0.012, 0.012, 13):
            gval = self.ring_gap(theta, psi0 + dp)
            if gval > bg: best, bg = psi0 + dp, gval
        if bg <= 0: return best, best, bg
        out = []
        for sgn in (-1, 1):
            lo, hi = 0.0, 0.03
            sh = sgn*self.cb/2
            for _ in range(22):
                mid = (lo + hi)/2
                if self.ring_gap(theta, best + sgn*mid, sh) > 0: lo = mid
                else: hi = mid
            out.append(best + sgn*lo)
        return out[0], out[1], bg

    def out_range(self, theta, psi):
        """Output carrier angle limits [phi-, phi+] for a disc at (theta, psi): each pin must stay inside its hole."""
        c = self.e*np.array([np.cos(theta), np.sin(theta)])
        cs, sn = np.cos(psi), np.sin(psi)
        H = c + np.stack([cs*self.holes[:, 0] - sn*self.holes[:, 1], sn*self.holes[:, 0] + cs*self.holes[:, 1]], 1)
        def ok(phi):
            c2, s2 = np.cos(phi), np.sin(phi)
            P = np.stack([c2*self.opins[:, 0] - s2*self.opins[:, 1], s2*self.opins[:, 0] + c2*self.opins[:, 1]], 1)
            return np.all(np.linalg.norm(P - H, axis=1) <= self.hole_r - RQ)
        res = []
        if not ok(psi):
            return psi, psi
        for sgn in (-1, 1):
            lo, hi = 0.0, 0.03
            for _ in range(22):
                mid = (lo + hi)/2
                if ok(psi + sgn*mid): lo = mid
                else: hi = mid
            res.append(psi + sgn*lo)
        return res[0], res[1]


def nominal_sign():
    """Which way the disc turns with the input, found from a perfect disc."""
    t = Trial(np.random.default_rng(0), perfect=True)
    th = 0.3
    gp = max(t.ring_gap(th, -th/(N - 1) + d) for d in (0,))
    gm = max(t.ring_gap(th, +th/(N - 1) + d) for d in (0,))
    return -1.0 if gp > gm else 1.0


def run_trial(rng, n_theta=RATIO*8, perfect=False, c_prof=C_PROFILE, c_hole=C_HOLE, tol=TOL):
    a = Trial(rng, tol, c_prof, c_hole, perfect); b = Trial(rng, tol, c_prof, c_hole, perfect)
    # the second disc runs on the SAME ring pins; cut stacked with the first (STACKED) it also has the same outline and the same holes
    b.pins = a.pins
    if STACKED: b.prof, b.tree, b.holes, b.hole_r = a.prof, a.tree, a.holes, a.hole_r
    # the error repeats 24 times per input turn (pin passing), so evenly spaced samples alias: a golden-ratio sequence over one output turn instead
    thetas = ((np.arange(n_theta)*0.6180339887498949) % 1.0)*2*np.pi*RATIO
    fwd, rev, play1 = [], [], []
    am = 180*60/np.pi
    bind = False
    for th in thetas:
        lo, hi, gmax = a.disc_range(th)
        if gmax <= 0: bind = True
        plo, _ = a.out_range(th, lo); _, phi = a.out_range(th, hi)
        nominal = SIGN*th/(N - 1)
        fwd.append(phi - nominal); play1.append(phi - plo)
        lo2, hi2, g2 = b.disc_range(th + np.pi)                       # second disc half a turn of the eccentric away
        if g2 <= 0: bind = True
        plo2, _ = b.out_range(th + np.pi, lo2)
        rev.append(plo2 - SIGN*(th + np.pi)/(N - 1))
    fwd, rev, play1 = np.array(fwd), np.array(rev), np.array(play1)
    gapc = fwd - rev                                                    # play left between disc A's forward stop and disc B's reverse stop
    # split the forward-flank error into what repeats slowly (1, 2, 3 per output turn), per input turn (23, 24, 46 per output turn) and the rest
    ph = thetas/RATIO
    def fit(ks):
        B = np.concatenate([np.ones((len(ph), 1))] + [np.stack([np.cos(k*ph), np.sin(k*ph)], 1) for k in ks], 1)
        c, *_ = np.linalg.lstsq(B, fwd, rcond=None); return B @ c
    slow = fit([1, 2, 3]); both = fit([1, 2, 3, RATIO, RATIO + 1, 2*RATIO]); rest = fwd - both
    pp = lambda v: float((v.max() - v.min())*am)
    return dict(bind=bind, lost_single_max=float(play1.max()*am), lost_single_mean=float(play1.mean()*am),
                te_pp=float((fwd.max() - fwd.min())*am), te_out_pp=pp(slow), te_in_pp=pp(both - slow), te_rest_pp=pp(rest),
                split_residual=float((gapc.max() - gapc.min())*am))


STACKED = os.environ.get("CYC_STACKED", "1") == "1"
SIGN = -1.0                                                  # found by nominal_sign(): the disc turns against the input


def _job(a):
    seed, tol, cp = a
    return run_trial(np.random.default_rng(seed), tol=tol, c_prof=cp, c_hole=cp, n_theta=RATIO*6)


if __name__ == "__main__":
    import multiprocessing as mp
    os.makedirs(OUT, exist_ok=True)
    assert SIGN == nominal_sign()
    POOL = mp.Pool(6)
    p0 = profile()
    k = curvature_radius(p0)
    # concave parts of the outline: where the curvature sign is opposite to that of the lobes' tips (largest radius points)
    rr = np.linalg.norm(p0, axis=1); tipsign = np.sign(np.median(k[rr > np.percentile(rr, 90)]))
    conc = k*tipsign < 0
    rho_min_concave = float(np.min(1/np.abs(k[conc]))) if conc.any() else float("inf")
    geo = dict(N=N, ratio=RATIO, R=R, pin_d=2*RP, e=E, shortening=E*N/R, disc_max_r=float(rr.max()), disc_min_r=float(rr.min()),
               min_concave_radius=rho_min_concave, cutter_r=TOOL_R, cuttable=rho_min_concave > TOOL_R + 0.3, lobe_height=2*E,
               pitch_lever_mm=E*N, out_pin_circle=RO, out_hole_d=2*(RQ + E + C_HOLE))
    rng = np.random.default_rng(20261120)
    perfect = run_trial(rng, perfect=True, c_prof=0.0, c_hole=0.0)
    perfect_clear = run_trial(rng, perfect=True)
    res = {}
    NT = int(os.environ.get("CYC_TRIALS", "60"))
    for label, tol in (("as milled, +/- 0.03 mm", 0.03), ("careful, +/- 0.015 mm", 0.015), ("probed and corrected, +/- 0.008 mm", 0.008), ("best case, +/- 0.004 mm", 0.004)):
        # the clearance cut into the profile and the holes: the smallest multiple of the tolerance at which fewer than 5 % of builds bind
        for mult in (1.5, 2.0, 2.5, 3.0, 3.5):
            cp = round(mult*tol, 4)
            rows = POOL.map(_job, [(int(rng.integers(1 << 31)), tol, cp) for _ in range(NT)])
            if np.mean([r["bind"] for r in rows]) < 0.05: break
        arr = lambda key: np.array([r[key] for r in rows])
        res[label] = dict(tol=tol, clearance=cp, trials=len(rows), bind_share=float(np.mean([r["bind"] for r in rows])),
                          lost_single_mean=float(arr("lost_single_mean").mean()), lost_single_p95=float(np.percentile(arr("lost_single_max"), 95)),
                          te_pp_mean=float(arr("te_pp").mean()), te_pp_p95=float(np.percentile(arr("te_pp"), 95)),
                          te_in_pp_mean=float(arr("te_in_pp").mean()), te_out_pp_mean=float(arr("te_out_pp").mean()), te_rest_pp_mean=float(arr("te_rest_pp").mean()), te_rest_pp_p95=float(np.percentile(arr("te_rest_pp"), 95)),
                          split_residual_mean=float(arr("split_residual").mean()), split_residual_p95=float(np.percentile(arr("split_residual"), 95)))
        print(label, res[label]); sys.stdout.flush()
    # --- stiffness, stress, efficiency (hand estimates)
    T = 36.0                                             # N·m: roughing peak at 46 mm
    lever = E*N/1000                                     # m: pitch-point lever of the ring mesh
    f_mesh = T/lever                                     # tangential force at the pitch point (shared by about N/3 pins when perfect; taken on 3)
    q_pin = f_mesh/3/math.cos(math.radians(40))
    L = 5.8                                              # mm contact length (6.35 disc less its edge breaks)
    # Hertz line contact, steel on steel, pin radius 4 against a concave flank of radius about 6: R* = 1/(1/4 - 1/6) = 12 mm
    Rstar = 1/(1/RP - 1/6.0); Estar = 210e3/(2*(1 - 0.3**2))
    sigma_h = math.sqrt(q_pin*Estar/(math.pi*Rstar*L))
    d_contact = 3.84e-5*q_pin**0.9/L**0.8                # mm approach per contact (Palmgren)
    k_contact = q_pin/(2*d_contact)*1000                 # N/m: pin-disc and pin-housing in series
    K_ring = 3*k_contact*lever**2
    k_opin = 3*210e9*(math.pi*0.010**4/64)/(0.007**3)    # output pin: 10 mm dowel, load 7 mm from its root
    K_out = NO*(1/(1/k_opin + 1/k_contact))*(RO/1000)**2
    K_brg = 1.0e8*lever**2                               # eccentric bearing radial stiffness 1e8 N/m (G), acting at the pitch lever
    K_total = 1/(1/K_ring + 1/K_out + 1/K_brg)
    f_ecc = (T/RATIO)/(E/1000)                           # N on the eccentric bearing (per disc when one disc carries the torque)
    est = dict(torque_nm=T, mesh_force_n=f_mesh, pin_load_n=q_pin, hertz_mpa=sigma_h, K_ring=K_ring, K_out=K_out, K_bearing=K_brg, K_total=K_total,
               eccentric_bearing_load_n=f_ecc, efficiency_est=0.75, backdrivable=True)
    json.dump(dict(geometry=geo, perfect_no_clearance=perfect, perfect_with_clearance=perfect_clear, monte_carlo=res, estimates=est, sign=SIGN,
                   assumptions=dict(TOL=TOL, C_PROFILE=C_PROFILE, C_HOLE=C_HOLE, bearing_clearance=CB)), open(os.path.join(OUT, "cycloid.json"), "w"), indent=1)
    md = ["# Cycloidal reducer: geometry and tolerance study\n", "Both discs on the same ring pins; discs " + ("cut stacked (same outline and holes)" if STACKED else "cut one at a time") + ".\n",
          f"- {N} ring pins of Ø{2*RP:.0f} on a Ø{2*R:.0f} circle, one lobe fewer on the disc: ratio {RATIO}:1. Eccentricity {E} mm (shortening {E*N/R:.2f}).",
          f"- Disc outline between R{rr.min():.2f} and R{rr.max():.2f}. Smallest concave radius {rho_min_concave:.2f} mm against the cutter's {TOOL_R}: {'cuttable' if geo['cuttable'] else 'NOT cuttable'}.",
          f"- Pitch lever e x N = {E*N:.1f} mm: 0.03 mm of play at the mesh is {0.03/(E*N)*180*60/math.pi:.1f} arc-min at the output.",
          "", "| Case | Clearance cut in (mm) | Builds that bind | Lost motion, one disc, no preload: mean / 95 % worst (arc-min) | Transmission error p-p: mean / 95 % (arc-min) | of which slow (1 to 3 per output turn) / per input turn / the rest (pin passing: what a coarse table cannot remove) | Play left with two discs twisted and locked: mean / 95 % (arc-min) |", "|---|---|---|---|---|---|---|",
          f"| Perfect parts, no clearance | 0 | {perfect['bind']} | {perfect['lost_single_mean']:.2f} / {perfect['lost_single_max']:.2f} | {perfect['te_pp']:.2f} | {perfect['te_out_pp']:.2f} / {perfect['te_in_pp']:.2f} / {perfect['te_rest_pp']:.2f} | {perfect['split_residual']:.2f} |",
          f"| Perfect parts, clearance {C_PROFILE} mm | {C_PROFILE} | {perfect_clear['bind']} | {perfect_clear['lost_single_mean']:.2f} / {perfect_clear['lost_single_max']:.2f} | {perfect_clear['te_pp']:.2f} | {perfect_clear['te_out_pp']:.2f} / {perfect_clear['te_in_pp']:.2f} / {perfect_clear['te_rest_pp']:.2f} | {perfect_clear['split_residual']:.2f} |"]
    for label, v in res.items():
        md.append(f"| {label}, {v['trials']} builds | {v['clearance']} | {v['bind_share']*100:.0f} % | {v['lost_single_mean']:.1f} / {v['lost_single_p95']:.1f} | {v['te_pp_mean']:.1f} / {v['te_pp_p95']:.1f} | {v['te_out_pp_mean']:.1f} / {v['te_in_pp_mean']:.1f} / {v['te_rest_pp_mean']:.1f} | {v['split_residual_mean']:.1f} / {v['split_residual_p95']:.1f} |")
    md += ["", f"Estimates at {T:.0f} N·m (C, G): mesh force {f_mesh:.0f} N, pin load {q_pin:.0f} N, contact stress {sigma_h:.0f} MPa, "
           f"torsional stiffness {K_total:.3g} N·m/rad (ring mesh {K_ring:.3g}, output pins {K_out:.3g}, eccentric bearing {K_brg:.3g}), eccentric bearing load {f_ecc:.0f} N."]
    open(os.path.join(OUT, "cycloid.md"), "w").write("\n".join(md))
    print("\n".join(md))
    try:
        import matplotlib; matplotlib.use("Agg"); import matplotlib.pyplot as plt
        fig, ax = plt.subplots(figsize=(7, 7)); ax.plot(p0[:, 0] + E, p0[:, 1], "k-", lw=1)
        a = 2*np.pi*np.arange(N)/N
        for xx, yy in zip(R*np.cos(a), R*np.sin(a)): ax.add_patch(plt.Circle((xx, yy), RP, fill=False, color="tab:blue"))
        ao = 2*np.pi*np.arange(6)/6
        for xx, yy in zip(RO*np.cos(ao), RO*np.sin(ao)): ax.add_patch(plt.Circle((xx + E, yy), RQ + E, fill=False, color="tab:red"))
        ax.set_aspect("equal"); ax.set_title(f"Cycloidal disc, {RATIO}:1, e = {E} mm, pins Ø{2*RP:.0f} on Ø{2*R:.0f}"); fig.savefig(os.path.join(OUT, "cycloid-profile.png"), dpi=130)
    except Exception as ex: print("no plot", ex)
