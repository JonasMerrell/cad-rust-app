"""Pack light meshes (merged per body and colour) and the results into one JSON blob for the viewer."""
import json, numpy as np, trimesh, base64, os, sys
D = os.path.join(os.environ.get('SIMOUT', 'sim-meshes'), '')   # folder written by `rcad script parts.rhai <folder> sim`
HERE = os.path.dirname(os.path.abspath(__file__))
sp = json.load(open(D + 'sim-parts.json'))
groups = {}
for p in sp['parts']:
    m = trimesh.load(D + p['file']); m.merge_vertices()
    nf = len(m.faces); target = int(min(nf, max(120, nf * 0.06)))
    if target < nf: m = m.simplify_quadric_decimation(face_count=target)
    groups.setdefault((p['group'], p['color']), []).append(m)
meshes = []
for (b, c), ms in groups.items():
    m = trimesh.util.concatenate(ms)
    v = np.round(np.asarray(m.vertices) * 40).astype(np.int16)    # 0.025 mm steps
    f = np.asarray(m.faces).astype(np.uint32)
    meshes.append(dict(body=b, color=c, v=base64.b64encode(v.tobytes()).decode(), f=base64.b64encode(f.tobytes()).decode()))
R = json.load(open(HERE + '/out/results.json'))
for k, run in R['cycle']['runs'].items():   # 50 fps for the viewer
    for key in ('t', 'seg', 'q', 'ratio', 'rpm', 'tau', 'coupling_sf', 'tcp_speed'):
        run[key] = run[key][::2]
R['workspace']['pts'] = R['workspace']['pts'][::2]
blob = dict(meshes=meshes, results=R, geom={k: sp[k] for k in ('H2', 'L1', 'L2', 'W', 'yF', 'xT')})
open(HERE + '/out/viewer-data.json', 'w').write(json.dumps(blob, separators=(',', ':')))
print('faces', sum(len(base64.b64decode(m['f'])) // 12 for m in meshes), 'MB', os.path.getsize(HERE + '/out/viewer-data.json') / 1e6, 'draw groups', len(meshes))
