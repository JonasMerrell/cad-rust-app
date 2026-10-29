//! The FE solver of [crackh3d-rs](https://github.com/JonasMerrell/crackh3d-rs) (commit 37e3a868, 2026-10-28):
//! element library, sparse assembly, CG + AMG and direct solves, and the general boundary conditions of
//! `fe::bcs` (fixed, displacement, symmetry, pressure, traction, force, spring, remote rigid / distributing,
//! point force, body force), plus periodic BCs, which crackh3d does not have.
//!
//! The modules are copied unchanged where they can be, so they stay easy to compare with crackh3d. The gmsh,
//! crack, SIF and growth code is left out; `progress`, `repro` and `eval_stats` are cut down to what the
//! solver calls.

pub mod elements;
pub mod eval_stats;
pub mod fe;
pub mod io;
pub mod mesh_data;
pub mod progress;
pub mod repro;
pub mod sif;
pub mod smallmat;
