"""Hand-calculation building blocks for the A axis (SI units unless a name says otherwise).

Every function is a documented textbook formula; the callers say which inputs are vendor data,
which are measured from the CAD, and which are guesses.
"""
import math
import numpy as np

# ---------------------------------------------------------------- cutting loads (rCAD's own model)
KR_ALU = 0.35          # radial / tangential force ratio, aluminium (rcad docs/cutting-model.md)
HELIX_SPREAD = 1.3     # rcad's factor on the sideways force for the helix
HELIX_DEG = 38.0       # typical 3-flute aluminium end mill helix (ASSUMED, 35-45 deg on the market)


def cutting(power_w, rpm, d_mm=6.35, flutes=3):
    """Forces at the tool from spindle cutting power, the way rCAD's Kienzle load check does it.
    F_t = P / v_c.  Sideways force on the tool (and so on the work) = F_t * 1.3 * sqrt(1 + kr^2)."""
    vc = math.pi*d_mm/1000*rpm/60                    # m/s
    ft = power_w/vc
    fr = KR_ALU*ft
    side = ft*HELIX_SPREAD*math.sqrt(1 + KR_ALU**2)
    fa = ft*math.tan(math.radians(HELIX_DEG))*0.5    # axial: about half of Ft*tan(helix) reaches the tool axis (Altintas, ch. 2)
    return dict(vc=vc, ft=ft, fr=fr, side=side, fa=fa, tooth_hz=rpm/60*flutes, torque_tool=ft*d_mm/2000)


def kienzle_peak_ratio(ae_mm, ap_mm, fz_mm, d_mm=6.35, flutes=3, kc1=700.0, mc=0.25):
    """Straight-tooth peak tangential force over the mean, for one cut shape (rCAD's formulas).
    Used to justify the dynamic factor for interrupted cutting."""
    phi = math.acos(max(-1.0, 1 - 2*ae_mm/d_mm))
    hmax = fz_mm*math.sin(phi) if phi < math.pi/2 else fz_mm
    hm = fz_mm*(1 - math.cos(phi))/phi
    kc = lambda h: kc1*max(h, 0.005)**(-mc)
    teeth = flutes*phi/(2*math.pi)
    f_mean = kc(hm)*ap_mm*hm*teeth
    f_peak = kc(hmax)*ap_mm*hmax*math.ceil(teeth)
    return dict(phi_deg=math.degrees(phi), f_mean=f_mean, f_peak=f_peak, ratio=f_peak/f_mean, teeth_in_cut=teeth)


# ---------------------------------------------------------------- crossed roller bearing
def palmgren_delta_mm(q_n, l_mm):
    """Approach of two steel bodies in line contact (Palmgren): delta = 3.84e-5 * Q^0.9 / l^0.8  [mm, N, mm]."""
    return 3.84e-5*q_n**0.9/l_mm**0.8


class CrossedRoller:
    """Crossed roller bearing: Z rollers of diameter Dw and effective length lw on pitch diameter dp,
    alternately at +45 and -45 degrees.  Each roller touches both rings: two line contacts in series.
    preload_um is the normal interference of each roller (0 = just touching, negative = clearance)."""

    def __init__(self, dp_mm, dw_mm, lw_mm, z, preload_um=2.0):
        self.dp, self.dw, self.lw, self.z, self.pre = dp_mm, dw_mm, lw_mm, z, preload_um/1000
        self.phi = np.arange(z)*2*np.pi/z
        self.sign = np.where(np.arange(z) % 2 == 0, 1.0, -1.0)      # which way the roller's axial line of action points

    def roller_loads(self, u):
        """u = [dy, dz (radial, mm), dx (axial, mm), ty, tz (tilt about y and z, rad)] of the inner ring.
        Returns normal load of each roller (N)."""
        dy, dz, dx, ty, tz = u
        rp = self.dp/2
        c = math.cos(math.pi/4)
        radial = dy*np.cos(self.phi) + dz*np.sin(self.phi)                   # outward radial motion at the roller
        axial = dx + rp*(ty*np.sin(self.phi) - tz*np.cos(self.phi))          # axial motion at the roller (x = bearing axis)
        approach = self.pre + c*(radial + self.sign*axial)                   # normal approach, mm
        a = np.maximum(approach, 0.0)
        # two contacts in series: approach = 2 * 3.84e-5 * Q^0.9 / l^0.8
        q = (a*self.lw**0.8/(2*3.84e-5))**(1/0.9)
        return q

    def forces(self, u):
        q = self.roller_loads(u)
        c = math.cos(math.pi/4); rp = self.dp/2
        fr = q*c; fa = q*c*self.sign
        fy = -np.sum(fr*np.cos(self.phi)); fz = -np.sum(fr*np.sin(self.phi)); fx = -np.sum(fa)
        my = -np.sum(fa*rp*np.sin(self.phi)); mz = np.sum(fa*rp*np.cos(self.phi))
        return np.array([fy, fz, fx, my, mz])       # N, N, N, N·mm, N·mm : reaction of the bearing on the inner ring

    def solve(self, load):
        """Displacement of the inner ring under an external load [Fy, Fz, Fx (N), My, Mz (N·mm)]."""
        from scipy.optimize import fsolve
        load = np.asarray(load, float)
        scale = np.array([1, 1, 1, 1/(self.dp/2), 1/(self.dp/2)])
        f = lambda u: (self.forces(u*np.array([1e-3, 1e-3, 1e-3, 1e-5, 1e-5])) + load)*scale
        u, info, ok, msg = fsolve(f, np.zeros(5), full_output=True)
        return u*np.array([1e-3, 1e-3, 1e-3, 1e-5, 1e-5]), ok == 1

    def stiffness(self, about=None):
        """Linearised 5x5 stiffness (N/mm, N·mm/rad) at the preload (or about a displacement state)."""
        u0 = np.zeros(5) if about is None else np.asarray(about, float)
        k = np.zeros((5, 5)); h = np.array([1e-5, 1e-5, 1e-5, 1e-7, 1e-7])
        for j in range(5):
            d = np.zeros(5); d[j] = h[j]
            k[:, j] = -(self.forces(u0 + d) - self.forces(u0 - d))/(2*h[j])
        return k

    def summary(self):
        k = self.stiffness()
        return dict(k_radial_N_per_um=k[0, 0]/1000, k_axial_N_per_um=k[2, 2]/1000, k_tilt_Nm_per_rad=k[3, 3]/1000,
                    preload_per_roller_N=float(self.roller_loads(np.zeros(5))[0]))


def bearing_life_h(c_dyn_n, p_equiv_n, rpm, roller=True):
    """ISO 281 basic rating life in hours."""
    p = 10/3 if roller else 3.0
    l10_rev = (c_dyn_n/p_equiv_n)**p*1e6
    return l10_rev/(rpm*60) if rpm > 0 else float("inf")


def crb_equivalent_load(fr, fa, m_nm, dp_mm):
    """THK/IKO crossed-roller dynamic equivalent radial load: P = X*(Fr + 2M/dp) + Y*Fa."""
    fre = fr + 2*m_nm/(dp_mm/1000)
    if fa/fre <= 1.5: x, y = 1.0, 0.45
    else: x, y = 0.67, 0.67
    return x*fre + y*fa


# ---------------------------------------------------------------- strain wave gear
def harmonic_windup_rad(t_nm, k1, k2, k3, t1, t2):
    """Three-slope torsional spring of a strain wave gear (Harmonic Drive catalogue method)."""
    t = abs(t_nm)
    if t <= t1: th = t/k1
    elif t <= t2: th = t1/k1 + (t - t1)/k2
    else: th = t1/k1 + (t2 - t1)/k2 + (t - t2)/k3
    return math.copysign(th, t_nm)


# ---------------------------------------------------------------- bolts
ISO_COARSE = {  # d: (pitch, stress area mm^2, head dia dk, head height k, clearance hole medium, hex key)
    3: (0.5, 5.03, 5.5, 3.0, 3.4, 2.5), 4: (0.7, 8.78, 7.0, 4.0, 4.5, 3.0), 5: (0.8, 14.2, 8.5, 5.0, 5.5, 4.0),
    6: (1.0, 20.1, 10.0, 6.0, 6.6, 5.0), 8: (1.25, 36.6, 13.0, 8.0, 9.0, 6.0), 10: (1.5, 58.0, 16.0, 10.0, 11.0, 8.0)}
PROOF = {"8.8": 580e6, "10.9": 830e6, "12.9": 970e6, "A2-70": 450e6}


def bolt(d, torque_nm, grade="12.9", k_nut=0.2):
    """Preload from tightening torque, F = T / (K d), and how much of the proof load that is."""
    p, a_s, dk, k, hole, key = ISO_COARSE[d]
    f = torque_nm/(k_nut*d/1000)
    proof = PROOF[grade]*a_s*1e-6
    return dict(preload_n=f, proof_n=proof, share_of_proof=f/proof)


def thread_strip_engagement(d, grade="12.9", shear_female_pa=0.6*240e6):
    """Thread engagement in a 6061-T6 tapped hole that develops the screw's proof load (shear of the internal thread,
    ISO 898 simplified: A_shear ~ 0.6 * pi * d * Le for the internal thread)."""
    p, a_s, *_ = ISO_COARSE[d]
    proof = PROOF[grade]*a_s*1e-6
    return proof/(0.6*math.pi*d/1000*shear_female_pa)*1000        # mm


def joint_slip_capacity(n_bolts, preload_n, mu=0.15):
    return n_bolts*preload_n*mu


def contact_stiffness_n_per_m(area_m2, pressure_pa, c=2.0e12, m=0.5):
    """Normal stiffness of a ground/milled bolted interface: k = c * p^m * A  (N/m), c in N/m^3 per Pa^m.
    Typical measured values for machined steel/aluminium faces at 2-10 MPa are 0.5-5e12 N/m^3 (Ito, 'Modular design
    for machine tools'); c*p^m with c=2e12/sqrt(1e6)... callers pass k_per_area directly instead."""
    raise NotImplementedError


def interface_k(area_mm2, k_per_area_n_per_mm3=1000.0):
    """Normal contact stiffness of a bolted machined interface.  1000 N/mm^3 (= 1 N/um per mm^2) is the low end of
    published values for milled aluminium on steel at a few MPa (500-5000 N/mm^3); GUESS, used as a conservative bound."""
    return area_mm2*k_per_area_n_per_mm3*1000        # N/m


# ---------------------------------------------------------------- simple structural pieces
def annulus_torsion_k(g_pa, t_m, ri_m, ro_m):
    """Torsional stiffness of a flat annular disc loaded in-plane between ri and ro: K = 4 pi G t / (1/ri^2 - 1/ro^2)."""
    return 4*math.pi*g_pa*t_m/(1/ri_m**2 - 1/ro_m**2)


def tube_torsion_k(g_pa, do_m, di_m, l_m):
    j = math.pi/32*(do_m**4 - di_m**4)
    return g_pa*j/l_m


def cantilever_tip(f_n, l_m, e_pa, i_m4): return f_n*l_m**3/(3*e_pa*i_m4)
def i_round(d_m): return math.pi*d_m**4/64
def i_rect(b_m, h_m): return b_m*h_m**3/12


def natural_hz(k, m): return math.sqrt(k/m)/(2*math.pi)
