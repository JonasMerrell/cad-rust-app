use crackh3d_fe::fe::bcs::{boundary_facets, boundary_facets_on_plane, solve_fe_with, structured_box_mesh, BcKind, BcSet, FeProblem, PeriodicSet, SurfFacet};
use crackh3d_fe::fe::solve::LinearSolver;

/// A unit cube periodic in x, y and z, stretched by a jump of 0.001 across x and pinned at one corner:
/// a uniform strain eps_xx = 0.001 with no strain across, so u_x = 0.001 x everywhere.
#[test]
fn unit_cell_takes_a_uniform_strain() {
    let mesh = structured_box_mesh([0.0; 3], [1.0; 3], [3, 3, 3], "tet10");
    let all = boundary_facets(&mesh);
    let side = |axis: usize, v: f64| boundary_facets_on_plane(&mesh, &all, axis, v, 1e-9);
    let eps = 1e-3;
    let periodic = (0..3)
        .map(|a| {
            let mut offset = [0.0; 3];
            offset[a] = 1.0;
            PeriodicSet { name: format!("periodic {a}"), leader: side(a, 0.0), follower: side(a, 1.0), offset, jump: if a == 0 { [eps, 0.0, 0.0] } else { [0.0; 3] } }
        })
        .collect();
    let corner = mesh.points.iter().position(|p| p.iter().all(|x| *x == 0.0)).unwrap();
    let pin = BcSet { name: "pin".into(), kind: BcKind::Fixed, facets: vec![SurfFacet::Tri6([corner; 6])] };
    let problem = FeProblem { bcs: vec![pin], periodic, ..Default::default() };
    for solver in [LinearSolver::Direct, LinearSolver::CgAmg { rtol: 1e-10 }] {
        let sol = solve_fe_with(&mesh, 200000.0, 0.3, &problem, solver).unwrap();
        for (p, u) in mesh.points.iter().zip(&sol.displacement) {
            assert!((u[0] - eps * p[0]).abs() < 1e-9 && u[1].abs() < 1e-9 && u[2].abs() < 1e-9, "{solver:?} at {p:?}: {u:?}");
        }
    }
}

#[test]
fn following_side_cannot_also_be_held() {
    let mesh = structured_box_mesh([0.0; 3], [1.0; 3], [2, 2, 2], "tet10");
    let all = boundary_facets(&mesh);
    let side = |axis: usize, v: f64| boundary_facets_on_plane(&mesh, &all, axis, v, 1e-9);
    let p = PeriodicSet { name: "periodic x".into(), leader: side(0, 0.0), follower: side(0, 1.0), offset: [1.0, 0.0, 0.0], jump: [0.0; 3] };
    let fixed = BcSet { name: "end".into(), kind: BcKind::Fixed, facets: side(0, 1.0) };
    let e = solve_fe_with(&mesh, 1.0, 0.3, &FeProblem { bcs: vec![fixed], periodic: vec![p], ..Default::default() }, LinearSolver::Direct).unwrap_err();
    assert!(e.to_string().contains("also held by end"), "{e}");
}
