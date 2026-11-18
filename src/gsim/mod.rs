//! G-code motion simulation for 3D printers: Klipper's config and macros, a G-code
//! interpreter and planner, and the printer's own CAD assembly driven through the moves with
//! collision, travel and printed-part checks.

pub mod cfg;
pub mod collide;
pub mod interp;
pub mod jinja;
pub mod plan;
pub mod print;
pub mod sim;
