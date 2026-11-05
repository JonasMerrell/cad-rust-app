# Writes project.json, the rCAD tool script that turns the exported STEPs into a project with a mated, driven assembly.
#   python3 make_project.py > project.json
import json, sys
E = 0.8; RATIO = 50
parts = [("Housing","printed-Housing.step",True),("Cycloid disc A","printed-Cycloid-disc-A.step",True),("Cycloid disc B","printed-Cycloid-disc-B.step",True),
("Cam A","printed-Cam-A.step",True),("Cam B","printed-Cam-B.step",True),("Carrier","printed-Carrier.step",True),("Output flange","printed-Output-flange.step",True),
("Cover","printed-Cover.step",True),("Test stand","printed-Test-stand.step",False),("Test lever","printed-Test-lever.step",True),
("Ring pins 3x20","bought-Ring-pins-3x20.step",True),("Output pins 6x30","bought-Output-pins-6x30.step",True),("Output bearing 6814","bought-Output-bearing-6814.step",True),
("Cam bearing 6805 A","bought-Cam-bearing-6805-A.step",True),("Cam bearing 6805 B","bought-Cam-bearing-6805-B.step",True),("Motor 17HS24","bought-Motor-17HS24.step",True),
("Tolerance coupon","printed-Tolerance-coupon.step",None),("Tolerance coupon 6814","printed-Tolerance-coupon-6814.step",None)]
calls = [{"tool":"new_document","args":{}}]
for i,(n,f,_) in enumerate(parts):
    if i: calls.append({"tool":"new_part_studio","args":{"name":n}})
    calls.append({"tool":"import_file","args":{"path":f,"name":n}})
calls.append({"tool":"create_assembly","args":{"name":"Test unit"}})
inst = {}
n_inst = 0
MULTI = {"Ring pins 3x20": 51, "Output pins 6x30": 6}   # studios with one body per pin: one instance each
pin_inst = {}
for i,(n,f,g) in enumerate(parts):
    if g is None: continue
    for b in range(MULTI.get(n, 1)):
        n_inst += 1
        a = {"assembly":0,"studio":i,"name":n if n not in MULTI else f"{n} #{b+1}"}
        if n in MULTI: a["body"] = [1, b]; pin_inst.setdefault(n, []).append(n_inst)
        else: inst[n] = n_inst
        if g: a.update(position=[0,0,100], rotate={"axis":[1,0,0],"angle":90})
        calls.append({"tool":"assembly_insert","args":a})
def c(n, o=(0,0,0), z=(0,0,1), x=(1,0,0)): return {"instance":inst[n] if isinstance(n,str) else n,"origin":list(o),"z":list(z),"x":list(x)}
mates = []
def mate(kind, a, b, name):
    mates.append(name)
    # ids are handed out by one counter: instances first, then mates in order
    calls.append({"tool":"assembly_mate","args":{"assembly":0,"kind":kind,"a":a,"b":b,"flip":True,"name":name}})
    return n_inst + len(mates)
# stator: everything bolted to the housing (housing frame = gearbox frame; the stand is in world coordinates)
mate("fastened", c("Housing"), c("Test stand", (0,0,100), (0,-1,0), (1,0,0)), "Housing to stand")
for n in ["Cover","Motor 17HS24"]: mate("fastened", c("Housing"), c(n), n + " to housing")
for k,i in enumerate(pin_inst["Ring pins 3x20"]): mate("fastened", c("Housing"), c(i), f"Ring pin {k+1} in housing")
# input: cam A turns on the motor axis; cam B is keyed to it by the D-flat
m_in = mate("revolute", c("Housing"), c("Cam A"), "Motor shaft (input)")
mate("fastened", c("Cam A"), c("Cam B"), "Cam B on cam A (D-flat)")
# discs ride their lobes on the 6805s
m_da = mate("revolute", c("Cam A", (E,0,0)), c("Cycloid disc A", (E,0,0)), "Disc A on lobe A")
m_db = mate("revolute", c("Cam B", (-E,0,0)), c("Cycloid disc B", (-E,0,0)), "Disc B on lobe B")
mate("fastened", c("Cycloid disc A", (E,0,0)), c("Cam bearing 6805 A", (E,0,0)), "6805 in disc A")
mate("fastened", c("Cycloid disc B", (-E,0,0)), c("Cam bearing 6805 B", (-E,0,0)), "6805 in disc B")
# output: carrier turns in the 6814
m_out = mate("revolute", c("Housing"), c("Carrier"), "Output bearing")
for n in ["Output bearing 6814","Output flange","Test lever"]: mate("fastened", c("Carrier"), c(n), n + " to carrier")
for k,i in enumerate(pin_inst["Output pins 6x30"]): mate("fastened", c("Carrier"), c(i), f"Output pin {k+1} in carrier")
# the cycloid kinematics: input θ turns the output by -θ/50, and each disc turns by -θ/50 absolute,
# so -51θ/50 relative to its lobe
SIGN = json.loads(sys.argv[1]) if len(sys.argv) > 1 else {"out": False, "disc": False}
calls.append({"tool":"assembly_relation","args":{"assembly":0,"kind":"gear","mate_a":m_in,"mate_b":m_out,"ratio":1.0/RATIO,"reverse":SIGN["out"]}})
for m in (m_da, m_db):
    calls.append({"tool":"assembly_relation","args":{"assembly":0,"kind":"gear","mate_a":m_in,"mate_b":m,"ratio":(RATIO+1.0)/RATIO,"reverse":SIGN["disc"]}})
if len(sys.argv) > 2:
    calls.append({"tool":"assembly_move","args":{"assembly":0,"instance":inst["Cam A"],"rotate":{"axis":[0,-1,0],"angle":float(sys.argv[2])}}})
calls.append({"tool":"assembly_info","args":{"assembly":0}})
calls.append({"tool":"render_assembly","args":{"assembly":0,"path":"project-check.png","view":"iso","width":1200,"height":900}})
calls.append({"tool":"save_document","args":{"path":"cycloidal-gearbox-prototype.rcad.json"}})
print('[\n' + ',\n'.join('  ' + json.dumps(x, ensure_ascii=False) for x in calls) + '\n]')
