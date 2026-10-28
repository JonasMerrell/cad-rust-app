//! Interactive web pages (see src/web_export.rs): meshes made from shapes here, the scene from Python as JSON.

use pyo3::prelude::*;

use crate::web_export;
use crate::{Shape, fail};

/// A shape's triangles and edges, ready for a web page.
#[pyclass(module = "cadrust._core", frozen)]
pub struct WebMesh {
    pub mesh: web_export::Mesh,
}

#[pymethods]
impl WebMesh {
    #[getter]
    fn triangle_count(&self) -> usize {
        self.mesh.indices.len() / 3
    }
}

/// Tessellate a shape for a web page (deflections: linear mm, angular radians), with its edges as segments.
#[pyfunction]
#[pyo3(signature = (shape, linear, angular, edges=true))]
pub fn web_mesh(shape: PyRef<'_, Shape>, linear: f64, angular: f64, edges: bool) -> PyResult<WebMesh> {
    let v = opencascade_sys::extra::ex_triangulate(shape.s(), linear, angular, false);
    if v.len() < 2 {
        return Err(fail("triangulate"));
    }
    let (nv, nt) = (v[0] as usize, v[1] as usize);
    let positions = v[2..2 + 3 * nv].iter().map(|x| *x as f32).collect();
    let indices = v[2 + 3 * nv..2 + 3 * nv + 3 * nt].iter().map(|x| *x as u32).collect();
    let mut segs = vec![];
    if edges {
        let e = opencascade_sys::extra::ex_edge_polylines(shape.s(), linear);
        let mut i = 1;
        while i < e.len() {
            let n = e[i] as usize;
            let pts = &e[i + 1..i + 1 + 3 * n];
            for k in 0..n.saturating_sub(1) {
                segs.extend(pts[3 * k..3 * k + 6].iter().map(|x| *x as f32));
            }
            i += 1 + 3 * n;
        }
    }
    Ok(WebMesh { mesh: web_export::Mesh { positions, indices, edges: segs } })
}

/// Write the page: `scene` is the JSON of web_export::Scene, its mesh numbers index `meshes`. Returns
/// (bytes written, meshes, triangles).
#[pyfunction]
#[pyo3(signature = (scene, meshes, path, inline=true))]
pub fn web_write(py: Python<'_>, scene: &str, meshes: Vec<PyRef<'_, WebMesh>>, path: &str, inline: bool) -> PyResult<(usize, usize, usize)> {
    let scene: web_export::Scene = serde_json::from_str(scene).map_err(|e| fail(&format!("web scene: {e}")))?;
    let meshes: Vec<web_export::Mesh> = meshes.iter().map(|m| m.mesh.clone()).collect();
    let w = py.detach(|| web_export::write(&scene, &meshes, std::path::Path::new(path), inline)).map_err(|e| fail(&e))?;
    Ok((w.bytes, w.meshes, w.triangles))
}
