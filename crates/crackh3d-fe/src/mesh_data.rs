//! Port of `crackh3d/mesh_data.py`: the in-memory mesh container shared by
//! every pipeline stage. Node ids are 0-based indices into `points`.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::elements::element_type;

#[derive(Debug, Error, PartialEq)]
pub enum MeshError {
    #[error("block '{0}': unknown element type '{1}'")]
    UnknownType(String, String),
    #[error("block '{name}': connectivity row length {got} does not match element type '{ty}' ({want} nodes/elem)")]
    Connectivity { name: String, ty: String, got: usize, want: usize },
    #[error("CrackFront.{0} length {1} != node_ids length {2}")]
    FrontLength(&'static str, usize, usize),
    #[error("crack_face_pos and crack_face_neg must both be set with the same length")]
    CrackFaces,
    #[error("point_data['{0}'] has {1} rows, expected {2}")]
    PointData(String, usize, usize),
    #[error("no element block named '{0}'")]
    NoBlock(String),
}

/// `ElementBlock`: connectivity is row-major, `num_nodes` ids per element.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementBlock {
    pub name: String,
    pub element_type: String,
    pub connectivity: Vec<Vec<usize>>,
    pub region_id: i64,
}

impl ElementBlock {
    pub fn new(name: &str, element_type_name: &str, connectivity: Vec<Vec<usize>>, region_id: i64) -> Result<Self, MeshError> {
        let et = element_type(element_type_name)
            .ok_or_else(|| MeshError::UnknownType(name.into(), element_type_name.into()))?;
        if let Some(row) = connectivity.iter().find(|r| r.len() != et.num_nodes) {
            return Err(MeshError::Connectivity { name: name.into(), ty: element_type_name.into(), got: row.len(), want: et.num_nodes });
        }
        Ok(Self { name: name.into(), element_type: element_type_name.into(), connectivity, region_id })
    }

    pub fn len(&self) -> usize {
        self.connectivity.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connectivity.is_empty()
    }
}

/// `CrackFront`: per-front-node frame, ordered along the front.
#[derive(Debug, Clone, PartialEq)]
pub struct CrackFront {
    pub node_ids: Vec<usize>,
    pub arc_length: Vec<f64>,
    pub tangent: Vec<[f64; 3]>,
    /// local x: in the crack plane, perpendicular to the tangent
    pub inplane_normal: Vec<[f64; 3]>,
    /// local y: crack-plane normal
    pub outofplane_normal: Vec<[f64; 3]>,
}

impl CrackFront {
    pub fn validate(&self) -> Result<(), MeshError> {
        let n = self.node_ids.len();
        for (name, len) in [
            ("arc_length", self.arc_length.len()),
            ("tangent", self.tangent.len()),
            ("inplane_normal", self.inplane_normal.len()),
            ("outofplane_normal", self.outofplane_normal.len()),
        ] {
            if len != n {
                return Err(MeshError::FrontLength(name, len, n));
            }
        }
        Ok(())
    }
}

/// `Mesh`. `point_data` fields are row-major `(N, k)` stored flat with their width.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub points: Vec<[f64; 3]>,
    pub blocks: Vec<ElementBlock>,
    pub crack_front: Option<CrackFront>,
    /// `(pos, neg)` node ids, paired index-for-index.
    pub crack_faces: Option<(Vec<usize>, Vec<usize>)>,
    pub region_names: BTreeMap<i64, String>,
    pub point_data: BTreeMap<String, (usize, Vec<f64>)>,
}

impl Mesh {
    pub fn validate(&self) -> Result<(), MeshError> {
        if let Some(f) = &self.crack_front {
            f.validate()?;
        }
        if let Some((p, n)) = &self.crack_faces {
            if p.len() != n.len() {
                return Err(MeshError::CrackFaces);
            }
        }
        for (name, (k, v)) in &self.point_data {
            let rows = if *k == 0 { 0 } else { v.len() / k };
            if rows != self.points.len() || v.len() != rows * k {
                return Err(MeshError::PointData(name.clone(), rows, self.points.len()));
            }
        }
        Ok(())
    }

    pub fn num_points(&self) -> usize {
        self.points.len()
    }

    pub fn num_elements(&self) -> usize {
        self.blocks.iter().map(ElementBlock::len).sum()
    }

    pub fn block_by_name(&self, name: &str) -> Result<&ElementBlock, MeshError> {
        self.blocks.iter().find(|b| b.name == name).ok_or_else(|| MeshError::NoBlock(name.into()))
    }

    /// Sorted unique node ids referenced by at least one block.
    pub fn used_node_ids(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = self.blocks.iter().flat_map(|b| b.connectivity.iter().flatten().copied()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}
