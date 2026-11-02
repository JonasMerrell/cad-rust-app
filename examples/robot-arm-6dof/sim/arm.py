"""Kinematics, dynamics and actuator model for the 6-axis arm (MuJoCo for rigid-body dynamics)."""
import numpy as np, mujoco, pickle, os
HERE = os.path.dirname(os.path.abspath(__file__))
JOINTS = ['J1', 'J2', 'J3', 'J4', 'J5', 'J6']
model = mujoco.MjModel.from_xml_path(HERE + '/arm.xml')
data = mujoco.MjData(model)
TCP = model.site('tcp').id
IFACE = model.site('iface').id
TOOL = model.body('tool').id

# ---------------------------------------------------------------- actuators
# Stepper data are typical datasheet values for the parts named in parts.rhai (assumed, not measured).
# T: holding torque N m, I: rated phase current A, R: ohm, L: H, Jr: rotor inertia kg m^2
MOTORS = {
    'J1': dict(name='17HS19 48 mm', T=0.45, I=1.68, R=1.65, L=3.2e-3, Jr=68e-7),
    'J2': dict(name='17HS24 60 mm', T=0.65, I=2.1, R=1.6, L=3.2e-3, Jr=82e-7 + 12e-7),  # + GT2 pulleys and shaft
    'J3': dict(name='17HS24 60 mm', T=0.65, I=2.1, R=1.6, L=3.2e-3, Jr=82e-7),
    'J4': dict(name='17HS16 40 mm', T=0.40, I=2.0, R=1.1, L=2.6e-3, Jr=54e-7),
    'J5': dict(name='17HS13 34 mm', T=0.28, I=1.33, R=2.1, L=4.0e-3, Jr=34e-7),
    'J6': dict(name='17HS4023 23 mm', T=0.13, I=1.0, R=3.5, L=3.5e-3, Jr=20e-7),
}
RATIO = dict(J1=30.0, J2=100.0, J3=30.0, J4=20.0, J5=20.0, J6=20.0)
EFF = dict(J1=0.75, J2=0.75 * 0.95, J3=0.75, J4=0.75, J5=0.75, J6=0.75)   # rcad's assumptions
VBUS = 24.0
POLE_PAIRS = 50
SAFETY = 0.7   # use at most 70 % of pull-out torque: open-loop steppers stall rather than slow down

DRIVER_IRMS = 1.8   # TMC2209: about 1.8 A RMS per phase is what it sustains with a heatsink

def drive_current(j):
    """RMS phase current the driver is set to: the motor's rating, capped by the TMC2209"""
    return min(MOTORS[j]['I'], DRIVER_IRMS)

def pullout(j, w):
    """pull-out torque (N m) of joint j's motor at motor speed w (rad/s). dq model of a 2-phase hybrid
    stepper, 50 pole pairs, 24 V bus, sine microstepping at drive_current(j) RMS; the best
    torque-producing current inside the voltage and current circles (the driver may lead the current)."""
    m = MOTORS[j]; w = np.abs(np.atleast_1d(np.asarray(w, float)))
    k = m['T'] / (np.sqrt(2) * m['I'])        # torque and back-EMF constant per phase (peak amps)
    Imax = np.sqrt(2) * drive_current(j)      # peak phase current
    E = k * w; X = POLE_PAIRS * w * m['L']; R = m['R']
    idd = np.linspace(-Imax, Imax, 81)[:, None, None]
    iq = np.linspace(0, Imax, 201)[None, :, None]
    re_ = E + R * iq - X * idd; im_ = X * iq + R * idd
    ok = (re_ ** 2 + im_ ** 2 <= VBUS ** 2) & (iq ** 2 + idd ** 2 <= Imax ** 2 + 1e-9)
    out = k * np.max(np.where(ok, iq, 0.0), axis=(0, 1))
    return out if out.size > 1 else float(out[0])

def motor_demand(q, qd, qdd):
    """per joint: motor speed (rad/s), motor torque needed (N m), joint torque (N m)"""
    tau = inverse(q, qd, qdd)
    N = np.array([RATIO[j] for j in JOINTS]); eta = np.array([EFF[j] for j in JOINTS])
    Jr = np.array([MOTORS[j]['Jr'] for j in JOINTS])
    wm = qd * N
    tm = np.abs(tau) / (N * eta) + Jr * np.abs(qdd) * N
    return wm, tm, tau

def usable_joint_torque(j, qd=0.0):
    return SAFETY * pullout(j, qd * RATIO[j]) * RATIO[j] * EFF[j]

# ---------------------------------------------------------------- dynamics
def fk(q):
    data.qpos[:] = q
    mujoco.mj_kinematics(model, data)
    mujoco.mj_comPos(model, data)
    return data

def inverse(q, qd, qdd):
    data.qpos[:] = q; data.qvel[:] = qd; data.qacc[:] = qdd
    mujoco.mj_inverse(model, data)
    return data.qfrc_inverse.copy()

def tool_wrench(q, qd, qdd):
    """force and moment (world) the arm applies to the tool at the interface face centre"""
    data.qpos[:] = q; data.qvel[:] = qd; data.qacc[:] = qdd
    mujoco.mj_inverse(model, data)
    mujoco.mj_rnePostConstraint(model, data)
    f = data.cfrc_int[TOOL].copy()      # [torque, force] from the parent, about the tree root's com, world frame
    com = data.subtree_com[model.body_rootid[TOOL]]
    F = f[3:]; Mc = f[:3]
    p = data.site_xpos[IFACE]
    M = Mc + np.cross(com - p, F)
    return F, M, data.site_xmat[IFACE].reshape(3, 3).copy()

def tcp(q):
    fk(q)
    return data.site_xpos[TCP].copy(), data.site_xmat[TCP].reshape(3, 3).copy()

def ik(target, axis_des, q0, iters=200, active=(0, 1, 2, 3, 4)):
    """position of the TCP + direction of the tool axis (5 constraints, J1..J5); J6 kept"""
    q = np.array(q0, float)
    jp = np.zeros((3, 6)); jr = np.zeros((3, 6))
    for _ in range(iters):
        p, R = tcp(q)
        ax = R[:, 0]
        e = np.concatenate([target - p, np.cross(ax, axis_des) * 0.3])
        if np.linalg.norm(e) < 1e-6: break
        mujoco.mj_jacSite(model, data, jp, jr, TCP)
        J = np.vstack([jp, jr * 0.3])[:, list(active)]
        lam = 1e-4
        dq = J.T @ np.linalg.solve(J @ J.T + lam * np.eye(6), e)
        q[list(active)] += np.clip(dq, -0.2, 0.2)
    return q, np.linalg.norm(e)
