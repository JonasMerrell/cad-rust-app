//! Toolpath simulation on a height map of the stock (seen from the spindle),
//! with crash checks: rapids into material, the shank or holder touching the
//! stock, and cuts into the part.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::cam::{Cam, OpKind, Tool, ToolKind};
use crate::cam_paths::{Move, Part, Toolpath};
use crate::cam_stock::{StockMap, EMPTY};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A rapid move cut material.
    Rapid,
    /// The shank (above the flutes) touched material.
    Shank,
    /// The holder touched material.
    Holder,
    /// The tool cut into the finished part.
    Gouge,
    /// The tool or its holder ran into a fixture (clamp, vise jaw).
    Fixture,
    /// The machine (head, spindle, table, column) ran into itself, the work, a fixture, or the tool into it.
    Machine,
    /// An axis went past its travel.
    Travel,
    /// A touch probe ran into material on its way (it should only ever touch it).
    Probe,
    /// The cutter took a cut too wide for how deep it is (or too deep for how wide): more load than
    /// the tool takes, as at a corner where a single contour meets a lump of stock.
    Overload,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Rapid => "rapid move into stock",
            EventKind::Shank => "shank hits the stock",
            EventKind::Holder => "holder hits the stock",
            EventKind::Gouge => "cuts into the part",
            EventKind::Fixture => "hits a fixture",
            EventKind::Machine => "machine crash",
            EventKind::Travel => "beyond the machine's travel",
            EventKind::Probe => "probe runs into the work",
            EventKind::Overload => "tool overloaded: the cut is too wide for its depth",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SimEvent {
    pub kind: EventKind,
    pub op: u64,
    /// Index into all moves of all toolpaths, in order.
    pub step: usize,
    /// Where (setup coordinates).
    pub at: [f64; 3],
    /// How deep, mm.
    pub depth: f64,
    /// What hit what (machine crashes, travel).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
}

#[derive(Clone)]
struct Cutter {
    radius: f64,
    /// Height of the cutting surface above the tip by distance from the axis, every LUT_STEP mm.
    lut: Vec<f32>,
    /// `lut_floor[i]`: no `lut_at` from step i out gives less (see `lut_floor`).
    lut_floor: Vec<f32>,
    /// Shank radius and bottom height above the tip.
    shank_r: f64,
    shank_z: f32,
    /// Cells under the holder (every other cell) and how far above the tip its underside is there.
    holder: Vec<(i32, i32, f32)>,
    /// How far out `holder` reaches (cells) and its lowest underside above the tip.
    holder_reach: i32,
    holder_low: f32,
    /// Cuts under an overhang (slot mills, dovetails, thread mills): the head takes material out of the
    /// middle of a column (see `Sim::voids`), and is checked against the part's solid, not its top.
    undercut: bool,
    /// An undercut cutter's head: its top above the tip by distance from the axis, every LUT_STEP mm
    /// (the head's full height on the neck: what is above that is for the shank check).
    head: Vec<f32>,
    /// Where an undercut tool's neck ends and the wider shank starts: (radius, height above the tip).
    upper: Option<(f64, f32)>,
    /// A dovetail's flank angle from its bottom face (radians), for the depth of a cut into the part.
    flank: f64,
    /// For a back chamfer: the heights above the tip where the flank may cut into the part's edge, and how far.
    chamfer_band: Option<(f32, f32, f32)>,
    /// How deep it may cut into the part without it counting (chamfers, taps, threads).
    allowance: f32,
    /// For chamfers and edge breaks: the cells where `allowance` holds (next to an edge whose face drops away
    /// at least as steeply as the cone); anywhere else a cut into the part is a gouge as for any tool.
    near_edge: Option<std::sync::Arc<Vec<bool>>>,
    /// The tool and holder as bands from the tip up: (radius, height of the band's bottom above the tip).
    body: Vec<(f64, f64)>,
    /// Length out of the holder, and the holder's length up to the spindle (for placing it in the machine).
    stickout: f64,
    holder_len: f64,
    /// A touch probe: any material in its way is a crash.
    probe: bool,
    /// An end mill whose load is checked (flat, bull, ball): its diameter, and the most cut cross-section it
    /// takes per mm of travel (mm², see `area_limit`).
    load_d: Option<(f64, f64)>,
    /// The tool for the physical cutting model, and its spindle speed (set in `push`).
    geom: crate::cam_cutting::ToolGeom,
    rpm: f64,
    /// A finishing pass: its bend is a wall error, not a load.
    finishing: bool,
    /// Coolant on for this toolpath (flood or mist): it washes chips out.
    coolant: bool,
    /// The tool's first bending mode, climb milling, and stable depths already worked out (by engagement
    /// and feed, both rounded).
    mode: crate::cam_cutting::Mode,
    climb: bool,
    stable: std::collections::HashMap<(u32, u32), f64>,
    /// The operation's own cut cross-section (step-down × stepover, mm²): a feed move taking under 5% of it
    /// is next to nothing.
    nominal: f64,
}

pub const LUT_STEP: f64 = 0.002;

/// The tool's height above its tip at `rho`, between the table's steps. Taking the step below instead (as
/// the simulation did) puts a steep flank too low: a 2.24° tapered ball rises 25.6 mm per mm out there, so
/// up to 0.05 mm, read as a gouge on a wall the flank only touches (the Eclipse car's tail corners).
pub fn lut_at(lut: &[f32], rho: f64) -> f32 {
    let x = (rho / LUT_STEP).max(0.0);
    let i = (x as usize).min(lut.len().saturating_sub(1));
    if i + 1 >= lut.len() {
        return lut[i];
    }
    let f = (x - i as f64).clamp(0.0, 1.0) as f32;
    lut[i] + (lut[i + 1] - lut[i]) * f
}

/// For each step of a tool's table, a height no `lut_at` at that step or further out goes under, as it is
/// computed in f32: the least of the table from there on. Where the table falls between two steps the
/// interpolation can round a hair under its lower end, so that step counts a micrometre lower.
fn lut_floor(lut: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; lut.len()];
    let mut least = f32::INFINITY;
    for i in (0..lut.len()).rev() {
        let mut here = lut[i];
        if i + 1 < lut.len() && lut[i + 1] < lut[i] {
            here = lut[i + 1] - 1e-3;
        }
        least = least.min(here);
        out[i] = least;
    }
    out
}

/// Height of a tool's cutting surface above its tip, every LUT_STEP mm out from the axis
/// (empty for tools that cut under an overhang).
pub fn tool_lut(t: &Tool) -> Vec<f32> {
    if t.kind.is_undercut() {
        return vec![];
    }
    let prof = t.profile();
    let r = t.radius();
    let height_at = |rho: f64| -> f64 {
        for w in prof.windows(2) {
            let (r0, z0) = w[0];
            let (r1, z1) = w[1];
            if rho >= r0 - 1e-9 && rho <= r1 + 1e-9 {
                if (r1 - r0).abs() < 1e-9 {
                    return z0.min(z1);
                }
                return z0 + (z1 - z0) * (rho - r0) / (r1 - r0);
            }
        }
        prof.last().map(|p| p.1).unwrap_or(0.0)
    };
    let exact = |rho: f64| crate::cam_3d::tool_height(t, rho).unwrap_or_else(|| height_at(rho));
    (0..=((r / LUT_STEP).ceil() as usize)).map(|i| exact((i as f64 * LUT_STEP).min(r)) as f32).collect()
}

/// The tool and holder as bands from the tip up: (radius, height of the band's bottom above the tip),
/// each reaching up to the next one's bottom (the last one has no top).
pub fn tool_body(t: &Tool) -> Vec<(f64, f64)> {
    let r = t.radius();
    let undercut = t.kind.is_undercut();
    let flutes = if undercut { t.head_height() } else { t.flute_length.max(r) };
    let neck = if undercut { t.neck_radius() } else { (t.shank_diameter / 2.0).max(0.1) };
    let stick = t.stickout.max(flutes + 1.0);
    let mut body = vec![(r, 0.0), (neck, flutes)];
    if let Some((sr, z)) = t.shank_step().filter(|s| s.1 < stick) {
        body.push((sr, z));
    }
    body.extend(t.holder_outline().iter().map(|(hr, h)| (*hr, stick + h)));
    body
}

impl Cutter {
    fn new(t: &Tool, cell: f64, allowance: f32) -> Cutter {
        let r = t.radius();
        let undercut = t.kind.is_undercut();
        let lut = tool_lut(t);
        let flutes = if undercut { t.head_height() } else { t.flute_length.max(r) };
        let neck = if undercut { t.neck_radius() } else { (t.shank_diameter / 2.0).max(0.1) };
        let stick = t.stickout.max(flutes + 1.0);
        let head: Vec<f32> = if undercut {
            // up to the head's top on the neck too: material above it there is the neck running into it
            (0..=((r / LUT_STEP).ceil() as usize)).map(|i| t.head_top((i as f64 * LUT_STEP).min(r)).unwrap_or(0.0).min(flutes) as f32).collect()
        } else {
            vec![]
        };
        let upper = t.shank_step().filter(|s| s.1 < stick).map(|(sr, z)| (sr, z as f32));
        let flank = if t.kind == ToolKind::Dovetail { t.dovetail_angle().to_radians() } else { std::f64::consts::FRAC_PI_2 };
        let outline = t.holder_outline();
        let reach = outline.iter().map(|p| p.0).fold(neck, f64::max);
        let n = (reach / (cell * 2.0)).ceil() as i32;
        let mut holder = vec![];
        for j in -n..=n {
            for i in -n..=n {
                let rho = ((i * i + j * j) as f64).sqrt() * cell * 2.0;
                if let Some(z) = crate::cam::holder_underside(&outline, rho.max(neck)) {
                    holder.push((i * 2, j * 2, (stick + z) as f32));
                }
            }
        }
        let body = tool_body(t);
        let lut_floor = lut_floor(&lut);
        let holder_reach = holder.iter().map(|h| h.0.abs().max(h.1.abs())).max().unwrap_or(0);
        let holder_low = holder.iter().map(|h| h.2).fold(f32::INFINITY, f32::min);
        Cutter { radius: r, lut, lut_floor, head, upper, flank, chamfer_band: None, shank_r: neck, shank_z: flutes as f32, holder, holder_reach, holder_low, undercut, allowance, near_edge: None, body, stickout: stick, holder_len: crate::cam_machine::holder_length(t), probe: t.kind == ToolKind::Probe, load_d: matches!(t.kind, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball | ToolKind::TaperedBall | ToolKind::Slot | ToolKind::Dovetail).then(|| (2.0 * r, area_limit(if undercut { 2.0 * neck } else { 2.0 * r }, &t.material))), nominal: 1.0, geom: crate::cam_cutting::tool_geom(t), rpm: 0.0, finishing: false, coolant: false, mode: crate::cam_cutting::Mode { k: 1.0, hz: 1.0, zeta: 0.03 }, climb: true, stable: Default::default() }
    }
}

pub const CLASS_STOCK: u8 = 0;
pub const CLASS_GOUGE: u8 = 1;
pub const CLASS_EARLIER: u8 = 2;
pub const CLASS_PATH: u8 = 3;

/// Colours of the toolpaths' cut surfaces, in order (repeating).
pub const PATH_COLOURS: [[u8; 3]; 8] =
    [[0x2f, 0x7d, 0xf6], [0x22, 0xb0, 0x5c], [0xf2, 0x8b, 0x1d], [0x9b, 0x4d, 0xd6], [0x14, 0xb3, 0xc4], [0xe0, 0x4a, 0x98], [0xc4, 0xa8, 0x10], [0x5b, 0x63, 0xe8]];

/// Colour of a mesh class: warm light stock, slate for earlier setups, red gouges, a colour per toolpath.
pub fn class_rgb(cl: u8) -> [u8; 3] {
    match cl {
        CLASS_STOCK => [0xd6, 0xcc, 0xb6],
        CLASS_GOUGE => [0xe5, 0x2b, 0x2b],
        CLASS_EARLIER => [0x8c, 0x98, 0xa6],
        k => PATH_COLOURS[(k - CLASS_PATH) as usize % PATH_COLOURS.len()],
    }
}

/// The stock in square blocks of `k` × `k` cells (the lowest cell of each): `mx` × `my` columns of material
/// [l, h] (h <= l: none), the colour class of each top and the cuts that exposed it (see `Sim::cuts`).
pub struct Blocks {
    pub k: usize,
    pub mx: usize,
    pub my: usize,
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub cls: Vec<u8>,
    pub cuts: Vec<[(u8, f32); 3]>,
    /// Gaps an undercut cutter left inside a block's column (see `Sim::voids`), by block.
    pub gaps: std::collections::HashMap<usize, Vec<(f64, f64)>>,
}

#[derive(Clone)]
pub struct Sim {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    /// Top of the material over each cell (EMPTY once cut through) and its bottom. Code outside that sets
    /// them calls `stock_changed` after.
    pub stock: Vec<f32>,
    pub lo: Vec<f32>,
    pub part: Vec<f32>,
    pub gouged: Vec<bool>,
    /// The last few toolpaths that cut each cell, newest first: (index + 1, 0 = none; the height the
    /// material was at when it started cutting there). Colours the walls by who exposed them.
    pub cuts: Vec<[(u8, f32); 3]>,
    pub top: f32,
    pub bottom: f32,
    pub moves: Vec<(usize, u64, Move)>,
    cutters: Vec<Cutter>,
    /// Next move to run.
    pub done: usize,
    pos: DVec3,
    pub events: Vec<SimEvent>,
    pub removed: f64,
    pub(crate) max_h: f32,
    /// A height no stock bottom (`lo`) is under: they only rise as undercut cutters take the bottom of a
    /// column, so the least of them when the stock was set stays one (`Work::floor`).
    floor: f32,
    /// Fixtures (setup coordinates).
    fixtures: Vec<(DVec3, DVec3)>,
    /// The highest fixture top: the tool's tip above it can't touch them.
    fixture_top: f64,
    /// The machine, when the setup has one.
    pub machine: Option<SimMachine>,
    /// The last millimetre or so of cutting with the current cutter, for the load check: (material
    /// removed mm³, horizontal travel mm, deepest cut mm) per tool position.
    load_win: std::collections::VecDeque<(f32, f64, f32)>,
    load_of: Option<(usize, u64)>,
    /// The heaviest cut of each operation (see `OpLoad`).
    pub loads: Vec<OpLoad>,
    /// How much of each operation's feed moves cut nothing (see `OpAir`).
    pub air: Vec<OpAir>,
    feed: f64,
    /// Air feed moves faster than this are links (`OpAir::link_s`): the machine's `max_feed`, which only
    /// link feeds go over.
    link_above: f64,
    /// In a tapping cycle: its travel isn't timed at a feed.
    tapping: bool,
    /// The stock material and the machine's limits for the cutting model; whether the limits are assumed.
    material: crate::cam_cutting::Material,
    pub limits: crate::cam_cutting::Limits,
    pub limits_assumed: bool,
    chatter_scale: f64,
    /// Chip evacuation (see `chip_sample`): chips lying in the cut on a coarse grid, mm³, when each cell was
    /// last brought up to date (s of cutting time), how deep the cavity there is; cutting time so far; the
    /// last chip's mean thickness; how walled-in the tool is (share of directions, wall height), re-measured
    /// every few positions.
    chips: crate::cam_cutting::ChipMap,
    time: f64,
    last_chip: f64,
    confinement: (f64, f64),
    conf_age: u32,
    last_width: f64,
    last_depth: f64,
    pub chip_ops: Vec<OpChips>,
    /// Each operation's closest approach to chatter (see `OpChatter`).
    pub chatter: Vec<OpChatter>,
    /// When set (`record_loads`), every load sample of every sideways cut, in order (the feed schedule).
    pub profile: Option<Vec<LoadSample>>,
    /// Gaps an undercut cutter left inside columns of stock, by cell: (bottom, top) going up, between the
    /// column's `lo` and its top.
    pub voids: std::collections::HashMap<u32, Vec<(f32, f32)>>,
    /// The part's solid by cell, for undercut cutters (worked out only when a toolpath uses one): its lowest
    /// surface (NEG_INFINITY: none) and the gaps between there and `part` (under overhangs).
    part_lo: Vec<f32>,
    part_gaps: std::collections::HashMap<u32, Vec<(f32, f32)>>,
    /// When set, material removed by each move (mm³, by move index) and the deepest cut it took (mm): which
    /// feed moves cut nothing (`cam_feed_schedule::link_feeds`).
    pub removed_by_move: Option<Vec<(f32, f32)>>,
    /// The stock in blocks of `BLOCK` x `BLOCK` cells, so a tool position skips the blocks it can't reach.
    bounds: Bounds,
}

/// Cells per side of a `Bounds` block.
const BLOCK: usize = 8;

/// For each block of cells, a height none of its stock is above: exact after `refresh`, and still above it
/// (never below) as cuts lower the stock, until the block is worked out again. A tool position whose cutting
/// surface over a block is at or above that height can't touch any cell in it, and the cell loops leave a cell
/// the tool doesn't touch as it is, so skipping those blocks changes nothing. Worked out from the stock when a
/// run starts on a stock array it wasn't made for, and whenever `start_from` sets the stock; the cell loops mark
/// the blocks they cut.
#[derive(Clone, Default)]
struct Bounds {
    nx: usize,
    ny: usize,
    top: Vec<f32>,
    stale: Vec<bool>,
    /// The stock array the bounds were worked out for (its address and length).
    of: (usize, usize),
    /// The blocks the last `reach` found within reach, row by row: first block column and row, columns.
    reach: Vec<bool>,
    at: (usize, usize, usize),
    /// Tests: every block within reach, as the loops were before the bounds.
    #[cfg(test)]
    off: bool,
}

impl Bounds {
    fn refresh(&mut self, stock: &[f32], nx: usize, ny: usize) {
        self.nx = nx.div_ceil(BLOCK);
        self.ny = ny.div_ceil(BLOCK);
        self.top = vec![EMPTY; self.nx * self.ny];
        self.stale = vec![false; self.nx * self.ny];
        for j in 0..ny {
            let row = (j / BLOCK) * self.nx;
            for i in 0..nx {
                let b = row + i / BLOCK;
                let s = stock[j * nx + i];
                if s > self.top[b] {
                    self.top[b] = s;
                }
            }
        }
        self.of = (stock.as_ptr() as usize, stock.len());
    }

    /// The height of block (bi, bj), worked out again if cuts lowered its stock since.
    fn top(&mut self, bi: usize, bj: usize, stock: &[f32], nx: usize, ny: usize) -> f32 {
        let b = bj * self.nx + bi;
        if self.stale[b] {
            let mut t = EMPTY;
            for j in bj * BLOCK..((bj + 1) * BLOCK).min(ny) {
                for &s in &stock[j * nx + bi * BLOCK..j * nx + ((bi + 1) * BLOCK).min(nx)] {
                    if s > t {
                        t = s;
                    }
                }
            }
            self.top[b] = t;
            self.stale[b] = false;
        }
        self.top[b]
    }

    fn lowered(&mut self, i: usize, j: usize) {
        self.stale[(j / BLOCK) * self.nx + i / BLOCK] = true;
    }

    /// Which blocks over the cells i0..=i1, j0..=j1 may have stock above `floor(rho)`, where `floor` gives a
    /// height the tool's surface at `p` is nowhere under from `rho` out, and `rho` is a block's nearest cell
    /// centre's distance from `p` (or less). Into `reach`, read with `reaches`.
    #[allow(clippy::too_many_arguments)]
    fn find_reach(&mut self, stock: &[f32], g: (f64, f64, f64, usize, usize), cells: (i32, i32, i32, i32), p: DVec3, floor: impl Fn(f64) -> f32) {
        let (x0, y0, cell, nx, ny) = g;
        self.reach.clear();
        if cells.0 > cells.1 || cells.2 > cells.3 {
            // off the grid: the cell loops have nothing to visit
            self.at = (0, 0, 0);
            return;
        }
        let (i0, i1, j0, j1) = (cells.0 as usize, cells.1 as usize, cells.2 as usize, cells.3 as usize);
        let (b0, b1, c0, c1) = (i0 / BLOCK, i1 / BLOCK, j0 / BLOCK, j1 / BLOCK);
        self.at = (b0, c0, b1 - b0 + 1);
        for bj in c0..=c1 {
            // the block's cell centres, as the cell loops work them out
            let (ya, yb) = (y0 + (bj * BLOCK).max(j0) as f64 * cell, y0 + ((bj + 1) * BLOCK - 1).min(j1) as f64 * cell);
            let dy = (ya - p.y).max(p.y - yb).max(0.0);
            for bi in b0..=b1 {
                let (xa, xb) = (x0 + (bi * BLOCK).max(i0) as f64 * cell, x0 + ((bi + 1) * BLOCK - 1).min(i1) as f64 * cell);
                let dx = (xa - p.x).max(p.x - xb).max(0.0);
                let top = self.top(bi, bj, stock, nx, ny);
                #[cfg(test)]
                let top = if self.off { f32::INFINITY } else { top };
                self.reach.push(top > floor((dx * dx + dy * dy).sqrt()));
            }
        }
    }

    /// A height none of the stock over cells i0..=i1, j0..=j1 is above (EMPTY for none).
    fn top_over(&mut self, stock: &[f32], nx: usize, ny: usize, cells: (i32, i32, i32, i32)) -> f32 {
        let (i0, i1, j0, j1) = (cells.0.max(0), cells.1.min(nx as i32 - 1), cells.2.max(0), cells.3.min(ny as i32 - 1));
        let mut t = EMPTY;
        #[cfg(test)]
        if self.off {
            return f32::INFINITY;
        }
        if i0 > i1 || j0 > j1 {
            return t;
        }
        for bj in j0 as usize / BLOCK..=j1 as usize / BLOCK {
            for bi in i0 as usize / BLOCK..=i1 as usize / BLOCK {
                t = t.max(self.top(bi, bj, stock, nx, ny));
            }
        }
        t
    }

    /// Whether the block of cell (i, j) is within reach (see `find_reach`).
    fn reaches(&self, i: i32, j: i32) -> bool {
        let (b0, c0, w) = self.at;
        self.reach[(j as usize / BLOCK - c0) * w + i as usize / BLOCK - b0]
    }
}

/// How much of [a, b] lies inside the column [lo, hi] once the gaps (sorted, disjoint) are taken out.
fn overlap(a: f32, b: f32, lo: f32, hi: f32, gaps: &[(f32, f32)]) -> f32 {
    let (a, b) = (a.max(lo), b.min(hi));
    if b <= a {
        return 0.0;
    }
    let mut v = b - a;
    for &(g0, g1) in gaps {
        let (x, y) = (g0.max(a), g1.min(b));
        if y > x {
            v -= y - x;
        }
    }
    v.max(0.0)
}

/// One tool position of a sideways cut as the load check sees it: the cut over about the last millimetre
/// (width, depth, cross-section), the move it belongs to, how far the tool went for it, and what the cutting
/// model makes of it at the move's feed.
#[derive(Debug, Clone, Copy)]
pub struct LoadSample {
    pub step: usize,
    pub width: f64,
    pub depth: f64,
    pub area: f64,
    pub len: f64,
    pub feed: f64,
    pub power: f64,
    pub worst: f64,
}

/// An operation's worst chatter margin: the cut's depth over the deepest stable depth at that
/// engagement and spindle speed (zero-order stability lobes, see `cam_cutting::stable_depth`); above 1 it
/// is expected to chatter. Trend-accurate only, until the tool's mode is measured.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct OpChatter {
    pub op: u64,
    pub margin: f64,
    pub at: [f64; 3],
    pub width: f64,
    pub depth: f64,
    pub stable: f64,
    pub rpm: f64,
    pub hz: f64,
    pub k: f64,
    /// What it takes to work the margin out again at another speed or stick-out.
    #[serde(skip)]
    pub kt: f64,
    #[serde(skip)]
    pub geom: Option<crate::cam_cutting::ToolGeom>,
    #[serde(skip)]
    pub zeta: f64,
    #[serde(skip)]
    pub climb: bool,
    #[serde(skip)]
    pub scale: f64,
}

impl OpChatter {
    /// The margin at another spindle speed, and with another stick-out (same holder; rCAD's `mode_k`/`mode_hz`
    /// measured values aren't scaled with stick-out).
    pub fn margin_at(&self, rpm: f64, stickout: Option<f64>, holder_k: f64, kr: f64) -> f64 {
        let Some(g) = self.geom else { return self.margin };
        let mode = match stickout {
            Some(s) => crate::cam_cutting::tool_mode(&crate::cam_cutting::ToolGeom { stickout: s, ..g }, holder_k, self.zeta),
            None => crate::cam_cutting::Mode { k: self.k, hz: self.hz, zeta: self.zeta },
        };
        let stable = crate::cam_cutting::stable_depth(&mode, g.diameter, self.width.max(0.05), g.flutes, self.kt, kr, rpm, self.climb).0;
        self.scale * self.depth / stable.max(1e-6)
    }
}

/// An operation's chip evacuation (a risk index, not chip physics: see docs/cutting-model.md): the most its
/// chips fill the cavity they lie in, where, and how its cutting time splits between open edges, slots
/// and pockets; how far it re-cut through packed chips, and cut full-width slots deeper than the diameter.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct OpChips {
    pub op: u64,
    pub max_fill: f64,
    pub at: [f64; 3],
    pub open_s: f64,
    pub slot_s: f64,
    pub pocket_s: f64,
    pub recut_mm: f64,
    pub deep_slot_mm: f64,
}

/// An operation's feed moves: how far, how long (at each move's own feed), and how much of that cut no
/// material at all (air, feed moves above the stock included) or next to none (under 5% of its own
/// step-down × stepover). `link_*` is the part of the air run faster than the machine's `max_feed` (link feeds).
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct OpAir {
    pub op: u64,
    pub feed_mm: f64,
    pub feed_s: f64,
    pub air_mm: f64,
    pub air_s: f64,
    pub light_mm: f64,
    pub light_s: f64,
    pub link_mm: f64,
    pub link_s: f64,
}

/// Where one operation's time goes, seconds: `time_s` (its `Toolpath::time`) = `cut_s` (feed moves that cut,
/// `light_s` of them barely, plus tapping) + `air_s` (feed moves that cut nothing, `link_s` of them at a
/// link feed) + `rapid_s` (its G0 moves) + `other_s` (dwells, and all of a probing cycle).
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct OpTime {
    pub op: u64,
    pub time_s: f64,
    pub cut_s: f64,
    pub light_s: f64,
    pub air_s: f64,
    pub link_s: f64,
    pub rapid_s: f64,
    pub other_s: f64,
}

/// A program's cycle time split up (see `time_split`): the operations' parts summed, and `between_s`, what the
/// cycle time adds to them (spindle spin-ups, the moves from one operation to the next, tool changes where
/// they're timed). The parts add up to `total_s`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct TimeSplit {
    pub total_s: f64,
    pub cut_s: f64,
    pub light_s: f64,
    pub air_s: f64,
    pub link_s: f64,
    pub rapid_s: f64,
    pub other_s: f64,
    pub between_s: f64,
    pub ops: Vec<OpTime>,
}

impl TimeSplit {
    /// `s` as a percentage of the cycle time.
    pub fn pct(&self, s: f64) -> f64 {
        if self.total_s > 0.0 {
            100.0 * s / self.total_s
        } else {
            0.0
        }
    }

    /// Time the tool isn't cutting: air feed, rapids and between operations.
    pub fn non_cutting_s(&self) -> f64 {
        self.air_s + self.rapid_s + self.between_s
    }

    /// The JSON the agent tools report.
    pub fn json(&self) -> serde_json::Value {
        let r1 = |x: f64| (x * 10.0).round() / 10.0;
        serde_json::json!({
            "air_cut_pct": r1(self.pct(self.air_s)),
            "link_pct": r1(self.pct(self.link_s)),
            "rapid_pct": r1(self.pct(self.rapid_s)),
            "between_ops_pct": r1(self.pct(self.between_s)),
            "cutting_pct": r1(self.pct(self.cut_s)),
            "light_cut_pct": r1(self.pct(self.light_s)),
            "other_pct": r1(self.pct(self.other_s)),
            "non_cutting_pct": r1(self.pct(self.non_cutting_s())),
            "seconds": {"total": r1(self.total_s), "cutting": r1(self.cut_s), "light_cut": r1(self.light_s), "air_cut": r1(self.air_s), "links": r1(self.link_s), "rapids": r1(self.rapid_s), "between_ops": r1(self.between_s), "other": r1(self.other_s)},
        })
    }
}

/// Split the cycle time `total` of `paths` (`cam_paths::cycle_time`, or a job's) into cutting, air cutting,
/// rapids and the rest, using the simulation's `air` for each operation's feed moves. Feed and rapid times
/// come from the toolpaths as their `time` does; the simulation gives the share of each operation's feed
/// time spent in air (and in links, light cuts), so the parts add up to the toolpath times exactly.
pub fn time_split(cam: &Cam, paths: &[Toolpath], air: &[OpAir], total: f64) -> TimeSplit {
    let mut t = TimeSplit { total_s: total, ..Default::default() };
    let mut sum = 0.0;
    for tp in paths.iter().filter(|p| !p.moves.is_empty()) {
        let Some(tool) = cam.path_tool(tp.op, tp.tool) else { continue };
        let m = crate::cam_paths::move_times(tp, &tool, cam);
        let mut o = OpTime { op: tp.op, time_s: tp.time, rapid_s: m.rapid_s, ..Default::default() };
        if tool.kind == ToolKind::Probe {
            o.rapid_s = 0.0;
            o.other_s = tp.time;
        } else {
            let share = |x: f64, of: f64| if of > 1e-9 { (x / of).clamp(0.0, 1.0) } else { 0.0 };
            let a = air.iter().find(|a| a.op == tp.op).copied().unwrap_or_default();
            o.air_s = m.feed_s * share(a.air_s, a.feed_s);
            o.link_s = m.feed_s * share(a.link_s, a.feed_s);
            o.light_s = m.feed_s * share(a.light_s, a.feed_s);
            o.cut_s = m.feed_s - o.air_s + m.tap_s;
            o.other_s = m.dwell_s;
        }
        sum += tp.time;
        t.cut_s += o.cut_s;
        t.light_s += o.light_s;
        t.air_s += o.air_s;
        t.link_s += o.link_s;
        t.rapid_s += o.rapid_s;
        t.other_s += o.other_s;
        t.ops.push(o);
    }
    t.between_s = (total - sum).max(0.0);
    t
}

/// How hard an operation works its tool at its heaviest: the cut's cross-section over about a
/// millimetre of travel, as a width (radial engagement) and depth (axial), against what the tool
/// takes at that width (`allowed_depth`) and in all (`area_limit`). `ratio` above 1 is an overload.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct OpLoad {
    pub op: u64,
    pub step: usize,
    pub at: [f64; 3],
    /// mm² of cut per mm of travel, width and depth (mm), tool diameter.
    pub area: f64,
    pub width: f64,
    pub depth: f64,
    pub diameter: f64,
    pub max_area: f64,
    /// The worst share of a limit the physical model gives (spindle power or torque, force, deflection,
    /// stress, chip): above 1 is an overload. `geometric` is the older width × depth rule.
    pub ratio: f64,
    pub geometric: f64,
    /// Feed and speed at that point, and what the cut does (see `cam_cutting`).
    pub feed: f64,
    pub rpm: f64,
    pub cut: crate::cam_cutting::Cut,
    pub uses: crate::cam_cutting::Use,
}

/// Machined part surface left with stock on it (see `Sim::unfinished`): how much area, the most left, where.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Unfinished {
    pub area_mm2: f64,
    pub max: f64,
    pub at: [f64; 3],
}

/// How deep an end mill of diameter `d` may cut, mm, when it is engaged `width` mm sideways: up to
/// 2.5 D at a light sideways step, 0.8 D at half its width, under half D in a full-width slot.
/// Calibrated on the DMC2 (a light mill, 6061): the first test part's cuts went fine (a 1/4" end mill
/// 4.65 wide × 3 deep, a 1.5 deep slot), the phone stand's single contour round the plinth corners,
/// 5 wide × 4 deep, overloaded the tool.
/// The most material an end mill of diameter `d` should remove per mm of travel, mm² (width × depth),
/// whatever the shape of the cut: 0.4 D² for carbide (16 mm² for a 1/4" end mill, a hair over the
/// test part's 13.9), 0.24 D² for HSS and cobalt.
pub fn area_limit(d: f64, material: &str) -> f64 {
    let k = if material.eq_ignore_ascii_case("carbide") || material.is_empty() { 0.4 } else { 0.24 };
    k * d * d
}

pub fn allowed_depth(d: f64, width: f64) -> f64 {
    const TABLE: [(f64, f64); 6] = [(0.0, 2.5), (0.15, 2.5), (0.3, 1.4), (0.5, 0.8), (0.75, 0.6), (1.0, 0.45)];
    let r = (width / d).clamp(0.0, 1.0);
    let k = TABLE.windows(2).find(|w| r <= w[1].0).map(|w| {
        let t = (r - w[0].0) / (w[1].0 - w[0].0);
        w[0].1 + t * (w[1].1 - w[0].1)
    });
    d * k.unwrap_or(TABLE[4].1)
}

/// The machine model placed for the simulation, and each toolpath's tool as points for checking it.
#[derive(Clone)]
pub struct SimMachine {
    pub placed: std::sync::Arc<crate::cam_machine::Placed>,
    fixtures: Vec<(String, DVec3, DVec3)>,
    /// Per cutter: how far the tip is below the spindle point, and points on the tool and holder.
    tools: Vec<Option<(f64, Vec<glam::Vec3>)>>,
    /// Travel since the last check, mm.
    since: f64,
    /// The machine parts in each crash: (move, part indices).
    pub hit_parts: Vec<(usize, Vec<usize>)>,
}

impl Sim {
    /// A fresh simulation of `paths` on the stock box (setup coordinates). `resolution` cells along the longer side.
    pub fn new(cam: &Cam, part: &Part, stock: (DVec3, DVec3), paths: &[Toolpath], resolution: usize) -> Sim {
        let (lo, hi) = stock;
        let size = (hi - lo).truncate();
        let cell = (size.x.max(size.y) / resolution.max(50) as f64).max(0.02);
        let pad = 0.0;
        let nx = ((size.x + 2.0 * pad) / cell).ceil() as usize + 1;
        let ny = ((size.y + 2.0 * pad) / cell).ceil() as usize + 1;
        let x0 = lo.x - pad;
        let y0 = lo.y - pad;
        let mut s = Sim {
            x0,
            y0,
            cell,
            nx,
            ny,
            stock: vec![hi.z as f32; nx * ny],
            lo: vec![lo.z as f32; nx * ny],
            part: vec![f32::NEG_INFINITY; nx * ny],
            gouged: vec![false; nx * ny],
            cuts: vec![[(0, 0.0); 3]; nx * ny],
            top: hi.z as f32,
            bottom: lo.z as f32,
            moves: vec![],
            cutters: vec![],
            done: 0,
            pos: DVec3::new(0.0, 0.0, hi.z + cam.clearance),
            events: vec![],
            removed: 0.0,
            max_h: hi.z as f32,
            floor: lo.z as f32,
            fixture_top: f64::INFINITY,
            machine: None,
            load_win: Default::default(),
            load_of: None,
            loads: vec![],
            air: vec![],
            feed: 1000.0,
            link_above: if cam.machine.max_feed > 0.0 { cam.machine.max_feed + 1.0 } else { f64::INFINITY },
            tapping: false,
            material: crate::cam_cutting::material(&cam.stock.material),
            limits: cam.machine.cut_limits().0,
            limits_assumed: cam.machine.cut_limits().1,
            chatter_scale: if cam.machine.chatter_scale > 0.0 { cam.machine.chatter_scale } else { crate::cam_cutting::CHATTER_SCALE },
            chips: crate::cam_cutting::ChipMap::new(stock.0.truncate(), stock.1.truncate(), 2.0, cam.machine.chip_clearing.clone()),
            time: 0.0,
            last_chip: 0.02,
            confinement: (0.0, 0.0),
            conf_age: 0,
            last_width: 0.0,
            last_depth: 0.0,
            chip_ops: vec![],
            chatter: vec![],
            profile: None,
            voids: Default::default(),
            part_lo: vec![],
            part_gaps: Default::default(),
            removed_by_move: None,
            bounds: Bounds::default(),
            fixtures: crate::cam_paths::fixture_boxes(cam, part, crate::cam_paths::work_origin(cam, part)).into_iter().map(|f| (f.1, f.2)).collect(),
        };
        // part height map: highest upward surface over each cell centre
        for t in &part.tris {
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            if n.z <= 1e-12 {
                continue;
            }
            let (mnx, mxx) = (t[0].x.min(t[1].x).min(t[2].x), t[0].x.max(t[1].x).max(t[2].x));
            let (mny, mxy) = (t[0].y.min(t[1].y).min(t[2].y), t[0].y.max(t[1].y).max(t[2].y));
            let i0 = (((mnx - x0) / cell).floor().max(0.0)) as usize;
            let i1 = (((mxx - x0) / cell).ceil().max(0.0) as usize).min(nx - 1);
            let j0 = (((mny - y0) / cell).floor().max(0.0)) as usize;
            let j1 = (((mxy - y0) / cell).ceil().max(0.0) as usize).min(ny - 1);
            let d = (t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y);
            if d.abs() < 1e-14 {
                continue;
            }
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let (px, py) = (x0 + i as f64 * cell, y0 + j as f64 * cell);
                    let a = ((t[1].x - px) * (t[2].y - py) - (t[2].x - px) * (t[1].y - py)) / d;
                    let b = ((t[2].x - px) * (t[0].y - py) - (t[0].x - px) * (t[2].y - py)) / d;
                    let c = 1.0 - a - b;
                    if a >= -1e-9 && b >= -1e-9 && c >= -1e-9 {
                        let z = (a * t[0].z + b * t[1].z + c * t[2].z) as f32;
                        let k = j * nx + i;
                        if z > s.part[k] {
                            s.part[k] = z;
                        }
                    }
                }
            }
        }
        s.fixture_top = s.fixtures.iter().map(|f| f.1.z).fold(f64::NEG_INFINITY, f64::max);
        if paths.iter().any(|tp| cam.path_tool(tp.op, tp.tool).is_some_and(|t| t.kind.is_undercut())) {
            s.part_solid(part);
        }
        for tp in paths {
            s.push(cam, tp);
        }
        s
    }

    /// The part's solid along each cell's vertical line (for undercut cutters): every crossing of its
    /// surface, paired off going up into stretches of material.
    fn part_solid(&mut self, part: &Part) {
        let (x0, y0, cell, nx, ny) = (self.x0, self.y0, self.cell, self.nx, self.ny);
        let mut hits: Vec<(u32, f32)> = vec![];
        for t in &part.tris {
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            if n.z.abs() <= 1e-12 {
                continue;
            }
            let (mnx, mxx) = (t[0].x.min(t[1].x).min(t[2].x), t[0].x.max(t[1].x).max(t[2].x));
            let (mny, mxy) = (t[0].y.min(t[1].y).min(t[2].y), t[0].y.max(t[1].y).max(t[2].y));
            let i0 = (((mnx - x0) / cell).floor().max(0.0)) as usize;
            let i1 = (((mxx - x0) / cell).ceil().max(0.0) as usize).min(nx - 1);
            let j0 = (((mny - y0) / cell).floor().max(0.0)) as usize;
            let j1 = (((mxy - y0) / cell).ceil().max(0.0) as usize).min(ny - 1);
            let d = (t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y);
            if d.abs() < 1e-14 || i0 > i1 || j0 > j1 {
                continue;
            }
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let (px, py) = (x0 + i as f64 * cell, y0 + j as f64 * cell);
                    let a = ((t[1].x - px) * (t[2].y - py) - (t[2].x - px) * (t[1].y - py)) / d;
                    let b = ((t[2].x - px) * (t[0].y - py) - (t[0].x - px) * (t[2].y - py)) / d;
                    let c = 1.0 - a - b;
                    // half-open edges, so a line through an edge shared by two triangles counts once
                    if a > -1e-12 && b > -1e-12 && c >= -1e-12 {
                        hits.push(((j * nx + i) as u32, (a * t[0].z + b * t[1].z + c * t[2].z) as f32));
                    }
                }
            }
        }
        hits.sort_by(|p, q| p.0.cmp(&q.0).then(p.1.total_cmp(&q.1)));
        self.part_lo = vec![f32::NEG_INFINITY; nx * ny];
        self.part_gaps.clear();
        let mut k = 0;
        while k < hits.len() {
            let cell = hits[k].0;
            let mut zs: Vec<f32> = vec![];
            while k < hits.len() && hits[k].0 == cell {
                // the same height twice (a vertex or edge hit from both sides) is one crossing
                if zs.last().is_none_or(|z| hits[k].1 - z > 1e-4) {
                    zs.push(hits[k].1);
                }
                k += 1;
            }
            if zs.len() < 2 {
                continue;
            }
            self.part_lo[cell as usize] = zs[0];
            // in, out, in, out …: the gaps are each out to the next in
            let gaps: Vec<(f32, f32)> = zs.chunks(2).collect::<Vec<_>>().windows(2).filter_map(|w| (w[0].len() == 2).then(|| (w[0][1], w[1][0]))).filter(|g| g.1 > g.0).collect();
            if !gaps.is_empty() {
                self.part_gaps.insert(cell, gaps);
            }
        }
    }

    /// Queue another toolpath to run after the ones already given.
    pub fn push(&mut self, cam: &Cam, tp: &Toolpath) {
        // every toolpath gets a cutter, so cutter indices match toolpath indices
        let Some(tool) = cam.path_tool(tp.op, tp.tool) else {
            self.cutters.push(Cutter::new(&Tool::default(), self.cell, 0.0));
            return;
        };
        let tool: &Tool = &tool;
        let op = cam.op(tp.op);
        let allowance = match op.map(|o| o.kind) {
            Some(OpKind::Chamfer) | Some(OpKind::Deburr) => op.map(|o| o.chamfer * 2.0 + 0.3).unwrap_or(1.0) as f32,
            Some(OpKind::Tap) | Some(OpKind::Thread) => f32::INFINITY,
            // engraving cuts into the part on purpose, as deep as asked
            Some(OpKind::Trace) => op.map(|o| o.depth + 0.05).unwrap_or(1.0) as f32,
            Some(OpKind::VCarve) => op.map(|o| if o.depth > 0.0 { o.depth + 0.05 } else { tool.flute_length }).unwrap_or(1.0) as f32,
            _ => 0.02,
        };
        let mut cutter = Cutter::new(tool, self.cell, allowance);
        // a dovetail's back chamfer: the flank cuts the edge on purpose, across the middle of its height
        if let Some(b) = op.filter(|o| o.kind == OpKind::Contour).and_then(|o| crate::cam_paths::back_chamfer(o, tool, 0.0)) {
            // the edge is `-b.tip` above the tip; the chamfer runs up the wall from there
            let (g, width) = (-b.tip, tool.radius() - (-b.tip) / tool.dovetail_angle().to_radians().tan() - b.offset);
            let rise = width * tool.dovetail_angle().to_radians().tan();
            cutter.chamfer_band = Some(((g - 0.05) as f32, (g + rise + 0.05) as f32, (width + 0.05) as f32));
        }
        if let Some(o) = op.filter(|o| matches!(o.kind, OpKind::Chamfer | OpKind::Deburr)) {
            cutter.near_edge = Some(std::sync::Arc::new(self.near_edges(o.chamfer.max(0.05))));
        }
        cutter.nominal = op.map(|o| crate::cam_paths::stepdown(o, tool) * crate::cam_paths::stepover(o, tool)).unwrap_or(1.0).max(0.01);
        cutter.rpm = cam.spindle_rpm(tool);
        cutter.coolant = cam.coolant_for(tool, op).is_on();
        cutter.climb = op.map(|o| o.climb).unwrap_or(true);
        let computed = crate::cam_cutting::tool_mode(&cutter.geom, cam.machine.holder_stiffness, if tool.mode_zeta > 0.0 { tool.mode_zeta } else { cam.machine.tool_damping });
        cutter.mode = crate::cam_cutting::Mode {
            k: if tool.mode_k > 0.0 { tool.mode_k } else { computed.k },
            hz: if tool.mode_hz > 0.0 { tool.mode_hz } else { computed.hz },
            zeta: computed.zeta,
        };
        cutter.finishing = op.is_some_and(|o| {
            matches!(o.kind, OpKind::Parallel | OpKind::Waterline | OpKind::Scallop | OpKind::Pencil) || (o.kind == OpKind::Contour && o.leave <= 0.0)
        });
        self.cutters.push(cutter);
        let ci = self.cutters.len() - 1;
        for m in &tp.moves {
            self.moves.push((ci, tp.op, *m));
        }
    }

    /// The machine placed for a setup (see `set_machine`; it takes a moment for a detailed machine, so it can be kept).
    pub fn place_machine(cam: &Cam, part: &Part, stock: (DVec3, DVec3), machine: std::sync::Arc<crate::cam_machine::Loaded>) -> crate::cam_machine::Placed {
        let fixtures = crate::cam_paths::fixture_boxes(cam, part, crate::cam_paths::work_origin(cam, part));
        let at = crate::cam_machine::Placed::reference(stock, &fixtures);
        crate::cam_machine::Placed::new(machine, at, DVec3::from(cam.on_table))
    }

    /// Check the machine too: its parts against each other and the work, the tool against the machine, and
    /// the axes against their travel. `offset` is where the setup sits on the table (see `Placed::new`).
    pub fn set_machine(&mut self, cam: &Cam, part: &Part, placed: std::sync::Arc<crate::cam_machine::Placed>) {
        let fixtures = crate::cam_paths::fixture_boxes(cam, part, crate::cam_paths::work_origin(cam, part));
        self.machine = Some(SimMachine { placed, fixtures, tools: vec![], since: f64::INFINITY, hit_parts: vec![] });
    }

    /// How far the tool tip of cutter `ci` is below the machine's spindle point.
    pub fn gauge(&self, ci: usize) -> f64 {
        let Some(c) = self.cutters.get(ci) else { return 0.0 };
        let holders = self.machine.as_ref().map(|m| m.placed.machine.model.holders).unwrap_or(false);
        c.stickout + if holders { c.holder_len } else { 0.0 }
    }

    fn check_machine(&mut self, ci: usize, op: u64, step: usize, p: DVec3) {
        let Some(mut m) = self.machine.take() else { return };
        if m.tools.len() <= ci {
            m.tools.resize(ci + 1, None);
        }
        if m.tools[ci].is_none() {
            let c = &self.cutters[ci];
            let gauge = c.stickout + if m.placed.machine.model.holders { c.holder_len } else { 0.0 };
            // the tool up to the spindle point (a model without holders has the collet nut on it already)
            m.tools[ci] = Some((gauge, crate::cam_machine::tool_points(&self.cutters[ci].body, gauge, 2.0)));
        }
        let (gauge, pts) = m.tools[ci].as_ref().unwrap();
        let q = m.placed.axes_at(p, *gauge);
        let over = m.placed.machine.model.over_travel(q);
        for (a, o) in over.iter().enumerate() {
            if *o > 1e-6 {
                let name = ["X", "Y", "Z"][a];
                let [lo, hi] = m.placed.machine.model.travel[a];
                let what = format!("{name} {} its travel ({lo:.0} to {hi:.0} mm from where it was modelled)", if q[a] < lo { "below" } else { "beyond" });
                self.event_what(EventKind::Travel, op, step, p, *o, what);
            }
        }
        let work = crate::cam_machine::Work {
            x0: self.x0,
            y0: self.y0,
            cell: self.cell,
            nx: self.nx,
            ny: self.ny,
            top: &self.stock,
            lo: &self.lo,
            part: &self.part,
            max_h: self.max_h,
            floor: self.floor,
            fixtures: &m.fixtures,
        };
        let hits = m.placed.check(p, *gauge, pts, &work);
        if !hits.is_empty() && m.hit_parts.len() < 2000 {
            let parts = hits.iter().flat_map(|h| h.mover.into_iter().chain(if let crate::cam_machine::Obstacle::Part(i) = h.what { Some(i) } else { None })).collect();
            m.hit_parts.push((step, parts));
        }
        for h in hits {
            let what = crate::cam_machine::describe(&m.placed.machine.model, &h);
            self.event_what(EventKind::Machine, op, step, h.at, h.depth, what);
        }
        self.machine = Some(m);
    }

    /// Start from stock left by an earlier setup instead of the full block.
    pub fn start_from(&mut self, m: &StockMap) {
        let mut top = EMPTY;
        for j in 0..self.ny {
            let y = self.y0 + j as f64 * self.cell;
            for i in 0..self.nx {
                let x = self.x0 + i as f64 * self.cell;
                let k = j * self.nx + i;
                match m.column(x, y) {
                    Some((lo, hi)) => {
                        self.lo[k] = lo;
                        self.stock[k] = hi;
                        top = top.max(hi);
                    }
                    None => self.stock[k] = EMPTY,
                }
            }
        }
        self.max_h = top.max(self.bottom);
        self.stock_changed();
    }

    /// Work out again what the run keeps about the stock (`bounds`, `floor`) after `stock` or `lo` were set
    /// from outside (a run read back from a cache, say). Cuts keep them up to date themselves.
    pub(crate) fn stock_changed(&mut self) {
        self.floor = self.lo.iter().copied().fold(f32::INFINITY, f32::min);
        self.bounds.refresh(&self.stock, self.nx, self.ny);
    }

    /// The stock as it is now.
    pub fn stock_map(&self) -> StockMap {
        StockMap { x0: self.x0, y0: self.y0, cell: self.cell, nx: self.nx, ny: self.ny, hi: self.stock.clone(), lo: self.lo.clone() }
    }

    /// A run that has finished, as bytes for the disk cache (`cam_sim_cache`): what it did to the stock and
    /// what it found. None when it hasn't finished, or records the load samples or each move's cut (the feed
    /// schedule's runs, which aren't kept).
    pub fn finished_bytes(&self) -> Option<Vec<u8>> {
        use crate::cam_sim_cache::Bin;
        if self.done < self.moves.len() || self.profile.is_some() || self.removed_by_move.is_some() {
            return None;
        }
        let mut w = Vec::with_capacity(self.stock.len() * 34 + 4096);
        self.nx.put(&mut w);
        self.ny.put(&mut w);
        self.moves.len().put(&mut w);
        [self.x0, self.y0, self.cell].put(&mut w);
        self.done.put(&mut w);
        self.pos.put(&mut w);
        self.events.put(&mut w);
        self.removed.put(&mut w);
        self.max_h.put(&mut w);
        self.loads.put(&mut w);
        self.air.put(&mut w);
        self.chip_ops.put(&mut w);
        self.chatter.put(&mut w);
        self.time.put(&mut w);
        let mut voids: Vec<(u32, Vec<(f32, f32)>)> = self.voids.iter().map(|(k, v)| (*k, v.clone())).collect();
        voids.sort_by_key(|v| v.0);
        voids.put(&mut w);
        self.machine.as_ref().map(|m| m.hit_parts.clone()).put(&mut w);
        for x in self.stock.iter().chain(&self.lo) {
            x.put(&mut w);
        }
        w.extend(self.gouged.iter().map(|g| *g as u8));
        for c in &self.cuts {
            c.put(&mut w);
        }
        Some(w)
    }

    /// Take the outcome of a finished run (`finished_bytes`) of this same simulation: this one fresh from
    /// `new` (and `set_machine`), on the same grid with the same moves. False (and this one untouched) when
    /// it doesn't fit.
    pub fn load_finished(&mut self, b: &[u8]) -> bool {
        use crate::cam_sim_cache::Bin;
        type Hits = Option<Vec<(usize, Vec<usize>)>>;
        let r = &mut &b[..];
        let n = self.nx * self.ny;
        let head = (|| -> Option<_> {
            let (nx, ny, moves, origin) = (usize::get(r)?, usize::get(r)?, usize::get(r)?, <[f64; 3]>::get(r)?);
            if (nx, ny, moves) != (self.nx, self.ny, self.moves.len()) || origin.map(f64::to_bits) != [self.x0, self.y0, self.cell].map(f64::to_bits) {
                return None;
            }
            let done = usize::get(r)?;
            let pos = DVec3::get(r)?;
            let events = Vec::<SimEvent>::get(r)?;
            let removed = f64::get(r)?;
            let max_h = f32::get(r)?;
            let loads = Vec::<OpLoad>::get(r)?;
            let air = Vec::<OpAir>::get(r)?;
            let chip_ops = Vec::<OpChips>::get(r)?;
            let chatter = Vec::<OpChatter>::get(r)?;
            let time = f64::get(r)?;
            let voids = Vec::<(u32, Vec<(f32, f32)>)>::get(r)?;
            let hits = Hits::get(r)?;
            Some((done, pos, events, removed, max_h, loads, air, chip_ops, chatter, time, voids, hits))
        })();
        let Some((done, pos, events, removed, max_h, loads, air, chip_ops, chatter, time, voids, hits)) = head else { return false };
        // the grids: two f32s, a bool and three (u8, f32) per cell
        if done != self.moves.len() || r.len() != n * (4 + 4 + 1 + 3 * 5) || hits.is_some() != self.machine.is_some() {
            return false;
        }
        let grid = |r: &mut &[u8]| (0..n).map(|_| f32::get(r)).collect::<Option<Vec<f32>>>();
        let (Some(stock), Some(lo)) = (grid(r), grid(r)) else { return false };
        let gouged: Vec<bool> = r[..n].iter().map(|g| *g != 0).collect();
        *r = &r[n..];
        let Some(cuts) = (0..n).map(|_| <[(u8, f32); 3]>::get(r)).collect::<Option<Vec<_>>>() else { return false };
        (self.stock, self.lo, self.gouged, self.cuts) = (stock, lo, gouged, cuts);
        (self.done, self.pos, self.events, self.removed, self.max_h) = (done, pos, events, removed, max_h);
        (self.loads, self.air, self.chip_ops, self.chatter, self.time) = (loads, air, chip_ops, chatter, time);
        self.voids = voids.into_iter().collect();
        if let (Some(m), Some(h)) = (self.machine.as_mut(), hits) {
            m.hit_parts = h;
        }
        true
    }

    pub fn total(&self) -> usize {
        self.moves.len()
    }

    /// The operation and move at step `k` of the run.
    pub fn move_at(&self, k: usize) -> Option<(u64, Move)> {
        self.moves.get(k).map(|m| (m.1, m.2))
    }

    pub fn tool_pos(&self) -> DVec3 {
        self.pos
    }

    /// Index of the toolpath cutter in use at the next move.
    pub fn current_cutter(&self) -> Option<usize> {
        self.moves.get(self.done.min(self.moves.len().saturating_sub(1))).map(|m| m.0)
    }

    /// Index of the toolpath cutter that made the last move (where the tool is now).
    pub fn last_cutter(&self) -> Option<usize> {
        match self.done.min(self.moves.len()).checked_sub(1) {
            Some(k) => self.moves.get(k).map(|m| m.0),
            None => self.current_cutter(),
        }
    }

    /// Run moves until `target` (exclusive) or until about `budget` tool positions have been stamped.
    /// Returns true when it reached the target.
    pub fn run_to(&mut self, target: usize, budget: usize) -> bool {
        let target = target.min(self.moves.len());
        if self.bounds.of != (self.stock.as_ptr() as usize, self.stock.len()) {
            self.bounds.refresh(&self.stock, self.nx, self.ny);
        }
        let mut spent = 0usize;
        while self.done < target {
            let (ci, op, m) = self.moves[self.done];
            let step = self.done;
            match m {
                Move::Rapid(p) => spent += self.travel(ci, op, step, DVec3::from(p), true),
                Move::Feed(p, f) => {
                    self.feed = f;
                    spent += self.travel(ci, op, step, DVec3::from(p), false)
                }
                Move::Tap { z, r, .. } => {
                    let a = self.pos;
                    self.tapping = true;
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, z), false);
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, r), false);
                    self.tapping = false;
                }
                Move::Dwell(_) => {}
            }
            self.done += 1;
            if spent >= budget {
                break;
            }
        }
        self.done >= target
    }

    fn travel(&mut self, ci: usize, op: u64, step: usize, to: DVec3, rapid: bool) -> usize {
        let from = self.pos;
        let len = from.distance(to);
        let n = ((len / self.cell).ceil() as usize).max(1);
        let check_every = ((1.0 / self.cell).ceil() as usize).max(1);
        let mut work = 0;
        for k in 1..=n {
            // from + (to - from)·t, not lerp's from·(1 - t) + to·t: that rounds a coordinate the move
            // doesn't change differently at every step, and a cell right on the cutter's rim then drops
            // in and out of its footprint along a plunge, leaving a sliver a later rapid "cuts"
            let p = if k == n { to } else { from + (to - from) * (k as f64 / n as f64) };
            if !self.fixtures.is_empty() {
                self.hit_fixtures(ci, op, step, p);
            }
            // (the first move comes from a made-up start above the setup's origin, not from where the machine
            // is: only its end is checked)
            if let Some(m) = self.machine.as_mut().filter(|_| step > 0 || k == n) {
                m.since += len / n as f64;
                if m.since >= m.placed.step || k == n {
                    m.since = 0.0;
                    self.check_machine(ci, op, step, p);
                }
            }
            // nothing to touch above the stock (a feed move there is air)
            if p.z as f32 > self.max_h + 1e-3 {
                if !rapid && !self.cutters[ci].probe {
                    self.air_sample(ci, op, 0.0, len / n as f64);
                }
                continue;
            }
            work += 1;
            // straight up only ever leaves material: it can't be a rapid into the stock (the tool can sit beside a
            // wall the grid shows a little inside it)
            let rising = to.z > from.z && (to - from).truncate().length() < 1e-9;
            let (removed, deepest) = self.stamp(ci, op, step, p, rapid && !rising, k % check_every == 0 || k == n);
            if self.removed_by_move.as_ref().is_some_and(|r| step < r.len()) {
                // material within two cells of the tool's side, at or above its tip: as good as touching
                let near = if removed > 0.0 { 1.0 } else { self.near_material(ci, p, 2.0 * self.cell + 0.05) };
                let r = &mut self.removed_by_move.as_mut().unwrap()[step];
                r.0 += removed;
                r.1 = r.1.max(deepest).max(near);
            }
            // plunges and steep ramps down cut with the tool's end: the load check is about sideways cuts
            // (a climb up a steep face cuts with the side: its travel is along the face)
            let (dxy, dz) = ((to - from).truncate().length() / n as f64, (to.z - from.z) / n as f64);
            if !rapid && !self.cutters[ci].probe {
                self.air_sample(ci, op, removed, dxy.hypot(dz));
            }
            if !rapid && dxy >= 2.0 * -dz {
                self.load_sample(ci, op, step, p, removed, deepest, dxy.hypot(dz));
            }
            if !rapid && !self.cutters[ci].probe {
                self.chip_sample(ci, op, p, removed, dxy.hypot(dz));
            }
        }
        self.pos = to;
        work
    }

    /// Whether the tool or holder at `p` overlaps a fixture.
    fn hit_fixtures(&mut self, ci: usize, op: u64, step: usize, p: DVec3) {
        let mut worst = 0.0f64;
        let body = &self.cutters[ci].body;
        if p.z >= self.fixture_top {
            return;
        }
        for (lo, hi) in &self.fixtures {
            let dx = (lo.x - p.x).max(p.x - hi.x).max(0.0);
            let dy = (lo.y - p.y).max(p.y - hi.y).max(0.0);
            let d = dx.hypot(dy);
            for (k, (r, z0)) in body.iter().enumerate() {
                let z1 = body.get(k + 1).map(|b| b.1).unwrap_or(f64::INFINITY);
                let (b0, b1) = (p.z + z0, p.z + z1);
                if d < *r - 1e-6 && b0 < hi.z - 1e-6 && b1 > lo.z {
                    worst = worst.max(hi.z - b0);
                }
            }
        }
        if worst > 0.0 {
            self.event(EventKind::Fixture, op, step, p, worst);
        }
    }

    /// sqrt(1 + slope²) of the part's height map at a cell (gentler of the two sides along each axis).
    /// Part surface this setup machined but left more than `tol` mm proud of (straight out from the
    /// surface): roughing that no finishing pass followed. (Seen from above: walls aren't measured.)
    pub fn unfinished(&self, tol: f32) -> Option<Unfinished> {
        let mut u = Unfinished { area_mm2: 0.0, max: 0.0, at: [0.0; 3] };
        for j in 0..self.ny {
            for i in 0..self.nx {
                let k = j * self.nx + i;
                let (s, p) = (self.stock[k], self.part[k]);
                if self.cuts[k][0].0 == 0 || !p.is_finite() || s == EMPTY || s <= p + tol {
                    continue;
                }
                let d = (s - p) / self.slope_factor(i as i32, j as i32);
                if d > tol {
                    u.area_mm2 += self.cell * self.cell;
                    if d as f64 > u.max {
                        u.max = d as f64;
                        u.at = [self.x0 + i as f64 * self.cell, self.y0 + j as f64 * self.cell, p as f64];
                    }
                }
            }
        }
        // a few cells is the grid, not a missing pass
        (u.area_mm2 > 1.0).then_some(u)
    }

    /// The cells within reach of a chamfer `w` wide of an edge: a cell whose part surface has, within
    /// 1.5 w + a cell, a neighbour lower by more than their distance apart plus w / 2 (a face falling away
    /// at 45° or steeper: a wall, a hole, a step).
    fn near_edges(&self, w: f64) -> Vec<bool> {
        let (nx, ny) = (self.nx as i32, self.ny as i32);
        let reach = 1.5 * w + self.cell;
        let n = (reach / self.cell).ceil() as i32;
        let mut out = vec![false; self.part.len()];
        for j in 0..ny {
            for i in 0..nx {
                let k = (j * nx + i) as usize;
                let h = self.part[k];
                if !h.is_finite() {
                    continue;
                }
                'near: for dj in -n..=n {
                    for di in -n..=n {
                        let d = ((di * di + dj * dj) as f64).sqrt() * self.cell;
                        if d > reach || (di == 0 && dj == 0) {
                            continue;
                        }
                        let (a, b) = (i + di, j + dj);
                        let low = if a < 0 || b < 0 || a >= nx || b >= ny { f32::NEG_INFINITY } else { self.part[(b * nx + a) as usize] };
                        if (h - low) as f64 > d + w / 2.0 {
                            out[k] = true;
                            break 'near;
                        }
                    }
                }
            }
        }
        out
    }

    fn slope_factor(&self, i: i32, j: i32) -> f32 {
        let (nx, ny) = (self.nx as i32, self.ny as i32);
        let h = |i: i32, j: i32| if i < 0 || j < 0 || i >= nx || j >= ny { f32::NEG_INFINITY } else { self.part[(j * nx + i) as usize] };
        let c = h(i, j);
        let side = |a: f32, b: f32| {
            let d = |q: f32| if q.is_finite() { (q - c).abs() } else { f32::INFINITY };
            let m = d(a).min(d(b));
            if m.is_finite() {
                m
            } else {
                0.0
            }
        };
        let cell = self.cell as f32;
        let gx = side(h(i - 1, j), h(i + 1, j)) / cell;
        let gy = side(h(i, j - 1), h(i, j + 1)) / cell;
        (1.0 + gx * gx + gy * gy).sqrt()
    }

    fn cell_of(&self, p: DVec3) -> (i32, i32) {
        (((p.x - self.x0) / self.cell).round() as i32, ((p.y - self.y0) / self.cell).round() as i32)
    }

    fn event(&mut self, kind: EventKind, op: u64, step: usize, at: DVec3, depth: f64) {
        self.event_what(kind, op, step, at, depth, String::new());
    }

    fn event_what(&mut self, kind: EventKind, op: u64, step: usize, at: DVec3, depth: f64, what: String) {
        // the same machine crash over and over in one operation is one problem (at its first move)
        if matches!(kind, EventKind::Machine | EventKind::Travel) {
            if let Some(e) = self.events.iter_mut().find(|e| e.kind == kind && e.op == op && e.what == what) {
                e.depth = e.depth.max(depth);
                return;
            }
        }
        if let Some(e) = self.events.iter_mut().rev().take(8).find(|e| e.kind == kind && e.op == op && e.what == what && (e.step == step || e.step + 1 == step)) {
            e.depth = e.depth.max(depth);
            e.step = step;
            return;
        }
        if self.events.len() < 500 {
            self.events.push(SimEvent { kind, op, step, at: at.to_array(), depth, what });
        }
    }

    fn air_sample(&mut self, ci: usize, op: u64, removed: f32, len: f64) {
        if len <= 0.0 || self.tapping {
            return;
        }
        let secs = len / (self.feed.max(1.0) / 60.0);
        let light = (removed as f64) / len < 0.05 * self.cutters[ci].nominal;
        let a = match self.air.iter_mut().find(|a| a.op == op) {
            Some(a) => a,
            None => {
                self.air.push(OpAir { op, ..Default::default() });
                self.air.last_mut().unwrap()
            }
        };
        a.feed_mm += len;
        a.feed_s += secs;
        if removed <= 1e-6 {
            a.air_mm += len;
            a.air_s += secs;
            if self.feed > self.link_above {
                a.link_mm += len;
                a.link_s += secs;
            }
        } else if light {
            a.light_mm += len;
            a.light_s += secs;
        }
    }

    /// Add a tool position to the load window and check the cut over the last millimetre.
    /// How walled-in the tool is at `p`: the share of 16 directions in which stock stands more than 1 mm
    /// above its tip within two diameters of its edge, and how high on average.
    fn walls(&self, p: DVec3, r: f64) -> (f64, f64) {
        let (mut walled, mut height) = (0, 0.0);
        for k in 0..16 {
            let a = k as f64 * std::f64::consts::PI / 8.0;
            let (dx, dy) = (a.cos(), a.sin());
            let mut top = f64::NEG_INFINITY;
            let mut d = r + 0.3;
            while d <= r + 4.0 * r {
                let (x, y) = (p.x + dx * d, p.y + dy * d);
                let i = ((x - self.x0) / self.cell).round();
                let j = ((y - self.y0) / self.cell).round();
                if i >= 0.0 && j >= 0.0 && (i as usize) < self.nx && (j as usize) < self.ny {
                    let h = self.stock[j as usize * self.nx + i as usize];
                    if h != EMPTY {
                        top = top.max(h as f64);
                    }
                }
                d += 0.5;
            }
            if top > p.z + 1.0 {
                walled += 1;
                height += top - p.z;
            }
        }
        (walled as f64 / 16.0, if walled > 0 { height / walled as f64 } else { 0.0 })
    }

    /// Chip evacuation, one tool position: see `cam_cutting::ChipMap`.
    fn chip_sample(&mut self, ci: usize, op: u64, p: DVec3, removed: f32, len: f64) {
        if len <= 0.0 {
            return;
        }
        let (r, d, coolant) = (self.cutters[ci].radius, 2.0 * self.cutters[ci].radius, self.cutters[ci].coolant);
        let dt = len / (self.feed.max(1.0) / 60.0);
        self.time += dt;
        self.conf_age += 1;
        if self.conf_age >= 6 || removed > 0.0 && self.confinement == (0.0, 0.0) {
            self.conf_age = 0;
            self.confinement = self.walls(p, r);
        }
        let (conf, depth) = self.confinement;
        let fill = self.chips.cut(p, removed as f64, conf, depth, self.last_chip, d, coolant, self.time);
        let rec = match self.chip_ops.iter_mut().find(|c| c.op == op) {
            Some(c) => c,
            None => {
                self.chip_ops.push(OpChips { op, ..Default::default() });
                self.chip_ops.last_mut().unwrap()
            }
        };
        if removed > 0.0 {
            match conf {
                c if c >= 0.75 => rec.pocket_s += dt,
                c if c >= 0.4 => rec.slot_s += dt,
                _ => rec.open_s += dt,
            }
        }
        if fill > rec.max_fill {
            rec.max_fill = fill;
            rec.at = p.to_array();
        }
        // cutting through packed chips: they get re-cut and welded on
        if removed > 0.0 && fill > 0.5 {
            rec.recut_mm += len;
        }
        if removed > 0.0 && self.last_width >= 0.9 * d && self.last_depth > d {
            rec.deep_slot_mm += len;
        }
    }

    fn load_sample(&mut self, ci: usize, op: u64, step: usize, p: DVec3, removed: f32, deepest: f32, dxy: f64) {
        let Some((d, max_area)) = self.cutters[ci].load_d else { return };
        if self.load_of != Some((ci, op)) {
            self.load_win.clear();
            self.load_of = Some((ci, op));
        }
        self.load_win.push_back((removed, dxy, deepest));
        let mut travel: f64 = self.load_win.iter().map(|w| w.1).sum();
        while self.load_win.len() > 1 && travel - self.load_win[0].1 >= 1.0 {
            travel -= self.load_win.pop_front().unwrap().1;
        }
        // a plunge or a short stretch says nothing about a sideways cut
        if travel < 0.5 {
            return;
        }
        let vol: f64 = self.load_win.iter().map(|w| w.0 as f64).sum();
        let depth = self.load_win.iter().map(|w| w.2).fold(0.0f32, f32::max) as f64;
        if depth < 0.05 {
            return;
        }
        let area = vol / travel;
        let width = (area / depth).min(d);
        let geometric = (depth / allowed_depth(d, width)).max(area / max_area);
        let c = &self.cutters[ci];
        let (feed, rpm) = (self.feed, c.rpm);
        let cut = crate::cam_cutting::cut(&c.geom, &self.material, width, depth, feed, rpm, area);
        let uses = crate::cam_cutting::use_of(&cut, &c.geom, &self.limits, rpm, c.finishing);
        let (mode, z, climb) = (c.mode, c.geom.flutes, c.climb);
        (self.last_chip, self.last_width, self.last_depth) = (cut.h_mean, width, depth);
        // chatter: this depth against the deepest stable one at this engagement and speed
        if rpm > 0.0 && width > 0.05 {
            let key = ((width / d * 20.0).round() as u32, (feed / 100.0).round() as u32);
            let kt = self.material.kc1 * cut.h_mean.max(0.005).powf(-self.material.mc);
            let kr = self.material.radial;
            let stable = *self.cutters[ci]
                .stable
                .entry(key)
                .or_insert_with(|| crate::cam_cutting::stable_depth(&mode, d, (key.0 as f64 / 20.0 * d).max(0.05), z, kt, kr, rpm, climb).0);
            let margin = self.chatter_scale * depth / stable.max(1e-6);
            let geom = Some(self.cutters[ci].geom);
            let rec = OpChatter { op, margin, at: p.to_array(), width, depth, stable, rpm, hz: mode.hz, k: mode.k, kt, geom, zeta: mode.zeta, climb, scale: self.chatter_scale };
            match self.chatter.iter_mut().find(|c| c.op == op) {
                Some(c) if c.margin < margin => *c = rec,
                Some(_) => {}
                None => self.chatter.push(rec),
            }
        }
        if let Some(prof) = self.profile.as_mut() {
            prof.push(LoadSample { step, width, depth, area, len: dxy, feed, power: cut.power, worst: uses.worst });
        }
        let ratio = uses.worst;
        let load = OpLoad { op, step, at: p.to_array(), area, width, depth, diameter: d, max_area, ratio, geometric, feed, rpm, cut, uses };
        match self.loads.iter_mut().find(|l| l.op == op) {
            Some(l) if l.ratio < ratio => *l = load,
            Some(_) => {}
            None => self.loads.push(load),
        }
        if ratio > 1.0 {
            self.event(EventKind::Overload, op, step, p, ratio);
        }
    }

    fn stamp(&mut self, ci: usize, op: u64, step: usize, p: DVec3, rapid: bool, check_holder: bool) -> (f32, f32) {
        let (ci0, cj0) = self.cell_of(p);
        let (nx, ny) = (self.nx as i32, self.ny as i32);
        let tip = p.z as f32;
        let area = (self.cell * self.cell) as f32;
        let mut removed = 0.0f32;
        let mut deepest = 0.0f32;
        let mut gouge = 0.0f32;
        let c = &self.cutters[ci];
        if !c.lut.is_empty() {
            let n = (c.radius / self.cell).ceil() as i32 + 1;
            let r2 = c.radius * c.radius;
            // the part's mesh cuts round holes with chords that sag up to about 0.025 mm inside the true
            // circle (a 29-sided Ø8 bore): a tool that just fits the hole would read as cutting those slivers
            let inner = (c.radius - 0.04).max(0.0);
            // a probe stops a hair short of a wall: within a cell of its ball's outline the stock map can't
            // tell that from a touch
            let probe_inner = (c.radius - self.cell).max(0.0);
            let cells = ((ci0 - n).max(0), (ci0 + n).min(nx - 1), (cj0 - n).max(0), (cj0 + n).min(ny - 1));
            // the blocks with stock above the cutting surface somewhere (a cell with none above it is one this
            // loop leaves as it is)
            let fl = &c.lut_floor;
            self.bounds.find_reach(&self.stock, (self.x0, self.y0, self.cell, self.nx, self.ny), cells, p, |rho| tip + fl[((rho / LUT_STEP).max(0.0) as usize).min(fl.len() - 1)]);
            for j in cells.2..=cells.3 {
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in cells.0..=cells.1 {
                    if !self.bounds.reaches(i, j) {
                        continue;
                    }
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    let d2 = dx * dx + dy * dy;
                    if d2 > r2 {
                        continue;
                    }
                    let rho = d2.sqrt();
                    let z = tip + lut_at(&c.lut, rho);
                    let k = (j * nx + i) as usize;
                    let s = self.stock[k];
                    if s > z {
                        let floor = self.lo[k];
                        let mut cut = s - z.max(floor);
                        // gaps an undercut cutter left in the column: no material there to take, and one the
                        // tip ends in opens the column down to its bottom
                        let mut top_z = z;
                        if !self.voids.is_empty() {
                            if let Some(v) = self.voids.get_mut(&(k as u32)) {
                                let zf = z.max(floor);
                                cut = (cut - v.iter().map(|g| (g.1.min(s) - g.0.max(zf)).max(0.0)).sum::<f32>()).max(0.0);
                                if let Some(g) = v.iter().find(|g| g.0 < z && g.1 >= z - 1e-4) {
                                    top_z = g.0;
                                }
                                v.retain(|g| g.1 < top_z);
                                if v.is_empty() {
                                    self.voids.remove(&(k as u32));
                                }
                            }
                        }
                        removed += cut * area;
                        if !c.probe || rho <= probe_inner {
                            deepest = deepest.max(cut);
                        }
                        // cut right through the column: nothing left over this cell
                        self.stock[k] = if top_z <= floor { EMPTY } else { top_z };
                        self.bounds.lowered(i as usize, j as usize);
                        let tag = (ci + 1).min(250) as u8;
                        let h = &mut self.cuts[k];
                        if h[0].0 != tag {
                            h.rotate_right(1);
                            h[0] = (tag, s);
                        }
                        let allowance = match &c.near_edge {
                            Some(m) if !m[k] => 0.02,
                            _ => c.allowance,
                        };
                        if rho <= inner && self.part[k] > z + allowance {
                            // depth across the surface, not straight down: on a steep wall a hair's
                            // breadth sideways reads as a deep cut from above
                            let d = (self.part[k] - z) / self.slope_factor(i, j);
                            if d > allowance {
                                gouge = gouge.max(d);
                                self.gouged[k] = true;
                            }
                        }
                    }
                }
            }
        }
        if c.undercut && !c.head.is_empty() {
            let n = (c.radius / self.cell).ceil() as i32 + 1;
            let r2 = c.radius * c.radius;
            let inner = (c.radius - 0.04).max(0.0);
            let tag = (ci + 1).min(250) as u8;
            let (cos_f, solid) = (c.flank.cos() as f32, !self.part_lo.is_empty());
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    let d2 = dx * dx + dy * dy;
                    if d2 > r2 {
                        continue;
                    }
                    let rho = d2.sqrt();
                    let rise = c.head[((rho / LUT_STEP) as usize).min(c.head.len() - 1)];
                    // the head takes [a, b] out of the column
                    let (a, b) = (tip, tip + rise);
                    let k = (j * nx + i) as usize;
                    let (s, floor) = (self.stock[k], self.lo[k]);
                    if s != EMPTY && b > floor && a < s {
                        let gaps = self.voids.get(&(k as u32)).map(|v| v.as_slice()).unwrap_or(&[]);
                        let cut = overlap(a, b, floor, s, gaps);
                        if cut > 0.0 {
                            removed += cut * area;
                            deepest = deepest.max(cut);
                            let h = &mut self.cuts[k];
                            if h[0].0 != tag {
                                h.rotate_right(1);
                                h[0] = (tag, s);
                            }
                        }
                        let (mut hi, mut lo) = (s, floor);
                        let mut v: Vec<(f32, f32)> = gaps.to_vec();
                        if b >= hi {
                            hi = a;
                        } else if a <= lo {
                            lo = b;
                        } else {
                            v.push((a, b));
                        }
                        // merge the gaps and fold the ones that reach the top or bottom into the column's ends
                        v.sort_by(|x, y| x.0.total_cmp(&y.0));
                        let mut m: Vec<(f32, f32)> = vec![];
                        for g in v {
                            match m.last_mut() {
                                Some(l) if g.0 <= l.1 => l.1 = l.1.max(g.1),
                                _ => m.push(g),
                            }
                        }
                        while m.last().is_some_and(|g| g.1 >= hi - 1e-4) {
                            hi = hi.min(m.pop().unwrap().0);
                        }
                        while m.first().is_some_and(|g| g.0 <= lo + 1e-4) {
                            lo = lo.max(m.remove(0).1);
                        }
                        m.retain(|g| g.0 > lo && g.1 < hi);
                        self.bounds.lowered(i as usize, j as usize);
                        if hi <= lo + 1e-4 {
                            self.stock[k] = EMPTY;
                            self.voids.remove(&(k as u32));
                        } else {
                            self.stock[k] = hi;
                            self.lo[k] = lo;
                            if m.is_empty() {
                                self.voids.remove(&(k as u32));
                            } else {
                                self.voids.insert(k as u32, m);
                            }
                        }
                    }
                    // into the part: the head against the part's solid along this line
                    if solid && rho <= inner && self.part_lo[k] > f32::NEG_INFINITY {
                        let pg = self.part_gaps.get(&(k as u32)).map(|v| v.as_slice()).unwrap_or(&[]);
                        let (pa, pb) = (a + 0.02, b - 0.02);
                        let into = overlap(pa, pb, self.part_lo[k], self.part[k], pg);
                        if into > 0.0 {
                            // through the bottom face it is as deep as it overlaps; through the flank, across it
                            let bottom = overlap(pa, pa + 1e-3, self.part_lo[k], self.part[k], pg) > 0.0;
                            let d = if bottom { into } else { into * cos_f };
                            // a back chamfer: the flank may cut the part where it is meant to take the edge off
                            let allowed = match c.chamfer_band {
                                Some((z0, z1, depth)) if !bottom => {
                                    let inside = overlap((tip + z0).max(pa), (tip + z1).min(pb), self.part_lo[k], self.part[k], pg).min(into);
                                    if (into - inside) * cos_f <= c.allowance { depth.max(c.allowance) } else { c.allowance }
                                }
                                _ => c.allowance,
                            };
                            if d > allowed {
                                gouge = gouge.max(d);
                                self.gouged[k] = true;
                            }
                        }
                    }
                }
            }
        }
        let mut shank_hit = 0.0f32;
        let mut holder_hit = 0.0f32;
        // (each check below looks for stock above a height: where the bounds say there is none over the cells it
        // looks at, it can't find any)
        if check_holder {
            let sz = tip + c.shank_z;
            // cells clearly inside the shank (it may rub a wall its own flutes cut)
            let rs = c.shank_r - 0.02;
            let n = (rs / self.cell).ceil() as i32;
            let clear = self.bounds.top_over(&self.stock, self.nx, self.ny, (ci0 - n, ci0 + n, cj0 - n, cj0 + n)) <= sz + 0.01;
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                if clear {
                    break;
                }
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    if dx * dx + dy * dy > rs * rs {
                        continue;
                    }
                    let s = self.stock[(j * nx + i) as usize];
                    if s > sz + 0.01 {
                        shank_hit = shank_hit.max(s - sz);
                    }
                }
            }
            // an undercut tool's full shank above its neck
            if let Some((ur, uz)) = c.upper {
                let (uz, rs) = (tip + uz, ur - 0.02);
                let n = (rs / self.cell).ceil() as i32;
                let clear = self.bounds.top_over(&self.stock, self.nx, self.ny, (ci0 - n, ci0 + n, cj0 - n, cj0 + n)) <= uz + 0.01;
                for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                    if clear {
                        break;
                    }
                    let dy = self.y0 + j as f64 * self.cell - p.y;
                    for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                        let dx = self.x0 + i as f64 * self.cell - p.x;
                        if dx * dx + dy * dy > rs * rs {
                            continue;
                        }
                        let s = self.stock[(j * nx + i) as usize];
                        if s != EMPTY && s > uz + 0.01 {
                            shank_hit = shank_hit.max(s - uz);
                        }
                    }
                }
            }
            let n = c.holder_reach;
            // the lowest of the holder's undersides, as each is worked out below (or lower)
            let clear = self.bounds.top_over(&self.stock, self.nx, self.ny, (ci0 - n, ci0 + n, cj0 - n, cj0 + n)) <= (tip + c.holder_low) + 0.01;
            for &(di, dj, dz) in if clear { &[][..] } else { &c.holder[..] } {
                let (i, j) = (ci0 + di, cj0 + dj);
                if i < 0 || j < 0 || i >= nx || j >= ny {
                    continue;
                }
                let s = self.stock[(j * nx + i) as usize];
                let hz = tip + dz;
                if s > hz + 0.01 {
                    holder_hit = holder_hit.max(s - hz);
                }
            }
        }
        let undercut = c.undercut;
        // a probe's stylus ball only comes within a hair of the work: deeper than a cell's worth of rounding is a hit
        if c.probe {
            if deepest as f64 > self.cell.max(0.05) {
                self.event(EventKind::Probe, op, step, p, deepest as f64);
            }
            gouge = 0.0;
        }
        self.removed += removed as f64;
        if rapid && deepest > 0.02 {
            self.event(EventKind::Rapid, op, step, p, deepest as f64);
        }
        if gouge > 0.0 {
            self.event(EventKind::Gouge, op, step, p, gouge as f64);
        }
        // an undercut cutter's neck may just graze the wall it is cutting under
        if shank_hit > 0.0 && !(undercut && shank_hit < 0.05) {
            self.event(EventKind::Shank, op, step, p, shank_hit as f64);
        }
        if holder_hit > 0.0 {
            self.event(EventKind::Holder, op, step, p, holder_hit as f64);
        }
        (removed, deepest)
    }

    /// The stock in blocks of cells, at most `max_cells` along each side (see `Blocks`).
    pub fn blocks(&self, max_cells: usize) -> Blocks {
        let k = ((self.nx.max(self.ny) as f64 / max_cells.max(10) as f64).ceil() as usize).max(1);
        let (mx, my) = (self.nx.div_ceil(k), self.ny.div_ceil(k));
        let bz = self.bottom as f64;
        // each block's column of material [lo, hi] (hi <= lo: none) and colour
        let mut h = vec![0.0f64; mx * my];
        let mut l = vec![0.0f64; mx * my];
        let mut cls = vec![0u8; mx * my];
        let mut cuts = vec![[(0u8, 0.0f32); 3]; mx * my];
        let mut gaps = std::collections::HashMap::new();
        for bj in 0..my {
            for bi in 0..mx {
                let mut top = f32::INFINITY;
                let mut low = self.bottom;
                let mut g = false;
                let mut by = [(0u8, 0.0f32); 3];
                let mut from: Option<usize> = None;
                for j in bj * k..((bj + 1) * k).min(self.ny) {
                    for i in bi * k..((bi + 1) * k).min(self.nx) {
                        let q = j * self.nx + i;
                        if self.stock[q] < top {
                            top = self.stock[q];
                            by = self.cuts[q];
                            low = self.lo[q];
                            from = Some(q);
                        }
                        g |= self.gouged[q];
                    }
                }
                let b = bj * mx + bi;
                if let Some(v) = from.and_then(|q| self.voids.get(&(q as u32))) {
                    gaps.insert(b, v.iter().map(|g| (g.0 as f64, g.1 as f64)).collect());
                }
                l[b] = (low as f64).max(bz);
                h[b] = if top == EMPTY { l[b] } else { (top as f64).max(l[b]) };
                cuts[b] = by;
                cls[b] = if g {
                    CLASS_GOUGE
                } else if by[0].0 > 0 {
                    CLASS_PATH.saturating_add(by[0].0 - 1)
                } else if top < self.top - 0.005 {
                    CLASS_EARLIER
                } else {
                    CLASS_STOCK
                };
            }
        }
        Blocks { k, mx, my, h, l, cls, cuts, gaps }
    }

    /// The tool and holder bands of the cutter of toolpath `ci` (see `tool_body`) and how far it sticks out of
    /// its holder.
    pub fn cutter_body(&self, ci: usize) -> Option<(&[(f64, f64)], f64)> {
        self.cutters.get(ci).map(|c| (c.body.as_slice(), c.stickout))
    }

    /// Index of the toolpath (and cutter) of move `k`.
    /// The cutting model's view of cutter `ci`: the tool, its spindle speed, whether it finishes.
    pub fn cutter_model(&self, ci: usize) -> Option<(crate::cam_cutting::ToolGeom, f64, bool)> {
        self.cutters.get(ci).map(|c| (c.geom, c.rpm, c.finishing))
    }

    /// 1 when stock stands at or above the tool tip within `slack` mm outside cutter `ci`'s radius at `p`, else 0
    /// (a link that close may take a cut on another grid).
    fn near_material(&self, ci: usize, p: DVec3, slack: f64) -> f32 {
        let c = &self.cutters[ci];
        let (ci0, cj0) = self.cell_of(p);
        let r = c.radius + slack;
        let n = (r / self.cell).ceil() as i32 + 1;
        let tip = p.z as f32 + 0.02;
        for j in (cj0 - n).max(0)..=(cj0 + n).min(self.ny as i32 - 1) {
            let dy = self.y0 + j as f64 * self.cell - p.y;
            for i in (ci0 - n).max(0)..=(ci0 + n).min(self.nx as i32 - 1) {
                let dx = self.x0 + i as f64 * self.cell - p.x;
                if dx * dx + dy * dy <= r * r {
                    let s = self.stock[(j * self.nx as i32 + i) as usize];
                    if s != EMPTY && s > tip {
                        return 1.0;
                    }
                }
            }
        }
        0.0
    }

    /// The material and the machine's limits the load check uses.
    pub fn cut_model(&self) -> (crate::cam_cutting::Material, crate::cam_cutting::Limits) {
        (self.material, self.limits)
    }

    pub fn cutter_at(&self, k: usize) -> Option<usize> {
        self.moves.get(k).map(|m| m.0)
    }

    /// Triangles of the simulated stock (setup coordinates), at most `max_cells` along each side:
    /// positions and per-triangle colour class: CLASS_STOCK (untouched), CLASS_GOUGE, CLASS_EARLIER
    /// (machined in an earlier setup) or CLASS_PATH + the index of the toolpath that cut it last.
    pub fn mesh(&self, max_cells: usize) -> (Vec<[DVec3; 3]>, Vec<u8>) {
        let Blocks { k, mx, my, h, l, cls, cuts, gaps } = self.blocks(max_cells);
        let no_gaps: Vec<(f64, f64)> = vec![];
        let gaps_of = |b: usize| gaps.get(&b).unwrap_or(&no_gaps);
        // the class of what cut a block's gaps: the newest toolpath that cut it
        let gap_class = |b: usize| if cuts[b][0].0 > 0 { CLASS_PATH.saturating_add(cuts[b][0].0 - 1) } else { CLASS_EARLIER };
        let bz = self.bottom as f64;
        let c = self.cell * k as f64;
        let mut tris = vec![];
        let mut col = vec![];
        let x_of = |i: usize| (self.x0 + i as f64 * c - self.cell / 2.0).min(self.x0 + (self.nx as f64 - 0.5) * self.cell);
        let y_of = |j: usize| (self.y0 + j as f64 * c - self.cell / 2.0).min(self.y0 + (self.ny as f64 - 0.5) * self.cell);
        let mut quad = |a: DVec3, b: DVec3, cc: DVec3, d: DVec3, cl: u8| {
            tris.push([a, b, cc]);
            tris.push([a, cc, d]);
            col.push(cl);
            col.push(cl);
        };
        let column = |ii: i64, jj: i64| -> Option<(f64, f64, u8)> {
            if ii < 0 || jj < 0 || ii >= mx as i64 || jj >= my as i64 {
                return None;
            }
            let b = jj as usize * mx + ii as usize;
            (h[b] > l[b] + 1e-4).then(|| (l[b], h[b], cls[b]))
        };
        for j in 0..my {
            for i in 0..mx {
                let Some((zl, z, cl)) = column(i as i64, j as i64) else { continue };
                let (xa, xb, ya, yb) = (x_of(i), x_of(i + 1), y_of(j), y_of(j + 1));
                quad(DVec3::new(xa, ya, z), DVec3::new(xb, ya, z), DVec3::new(xb, yb, z), DVec3::new(xa, yb, z), cl);
                // underneath: the stock's bottom, or a surface an earlier setup machined
                let under = if zl > bz + 1e-3 { CLASS_EARLIER } else { CLASS_STOCK };
                quad(DVec3::new(xa, ya, zl), DVec3::new(xa, yb, zl), DVec3::new(xb, yb, zl), DVec3::new(xb, ya, zl), under);
                // gaps an undercut cutter left inside the column: their ceiling and floor
                let b = j * mx + i;
                let own = gaps_of(b);
                for &(g0, g1) in own {
                    let gc = gap_class(b);
                    quad(DVec3::new(xa, ya, g1), DVec3::new(xa, yb, g1), DVec3::new(xb, yb, g1), DVec3::new(xb, ya, g1), gc);
                    quad(DVec3::new(xa, ya, g0), DVec3::new(xb, ya, g0), DVec3::new(xb, yb, g0), DVec3::new(xa, yb, g0), gc);
                }
                // sides: the parts of [zl, z] the neighbour doesn't cover. Above it they were exposed by the cuts
                // that took the neighbour down (newest lowest), below it by an earlier setup; the stock's outer
                // faces keep the stock's colour.
                for (di, dj) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                    let (ni, nj) = (i as i64 + di, j as i64 + dj);
                    let mut segs = vec![];
                    if ni < 0 || nj < 0 || ni >= mx as i64 || nj >= my as i64 {
                        segs.push((zl, z, if cl == CLASS_EARLIER { CLASS_EARLIER } else { CLASS_STOCK }));
                    } else {
                        let nb = nj as usize * mx + ni as usize;
                        let solid = h[nb] > l[nb] + 1e-4;
                        let (nl, nh) = if solid { (l[nb], h[nb]) } else { (f64::INFINITY, f64::NEG_INFINITY) };
                        if nh < z {
                            // a neighbour cut right through still had material only from its bottom up
                            let mut at = if solid { nh.max(zl) } else { l[nb].max(zl) };
                            for (tag, from) in cuts[nb] {
                                if tag == 0 {
                                    break;
                                }
                                let up = (from as f64).min(z);
                                if up > at {
                                    let c = if cls[nb] == CLASS_GOUGE && at <= nh.max(zl) { CLASS_GOUGE } else { CLASS_PATH.saturating_add(tag - 1) };
                                    segs.push((at, up, c));
                                    at = up;
                                }
                            }
                            if at < z {
                                segs.push((at, z, CLASS_EARLIER));
                            }
                        }
                        let below = if solid { nl } else { l[nb] };
                        if below > zl {
                            segs.push((zl, below.min(z), CLASS_EARLIER));
                        }
                        // the neighbour's own gaps open this side too
                        if solid {
                            for &(g0, g1) in gaps_of(nb) {
                                let (a, b) = (g0.max(zl).max(nl), g1.min(z).min(nh));
                                if b > a {
                                    segs.push((a, b, gap_class(nb)));
                                }
                            }
                        }
                    }
                    // nothing to draw across this column's own gaps
                    if !own.is_empty() {
                        let mut cut = vec![];
                        for (a, b, c) in segs {
                            let mut at = a;
                            for &(g0, g1) in own {
                                if g1 <= at || g0 >= b {
                                    continue;
                                }
                                if g0 > at {
                                    cut.push((at, g0, c));
                                }
                                at = at.max(g1);
                            }
                            if at < b {
                                cut.push((at, b, c));
                            }
                        }
                        segs = cut;
                    }
                    for (a, b, c) in segs {
                        if b <= a + 1e-6 {
                            continue;
                        }
                        match (di, dj) {
                            (1, 0) => quad(DVec3::new(xb, ya, a), DVec3::new(xb, yb, a), DVec3::new(xb, yb, b), DVec3::new(xb, ya, b), c),
                            (-1, 0) => quad(DVec3::new(xa, yb, a), DVec3::new(xa, ya, a), DVec3::new(xa, ya, b), DVec3::new(xa, yb, b), c),
                            (0, 1) => quad(DVec3::new(xb, yb, a), DVec3::new(xa, yb, a), DVec3::new(xa, yb, b), DVec3::new(xb, yb, b), c),
                            _ => quad(DVec3::new(xa, ya, a), DVec3::new(xb, ya, a), DVec3::new(xb, ya, b), DVec3::new(xa, ya, b), c),
                        }
                    }
                }
            }
        }
        (tris, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;

    /// The blocks the cell loops skip change nothing: the same cuts with every block looked at (as before the
    /// bounds) leave the same stock, bit for bit, and find the same problems, loads and air. A flat end mill
    /// roughs into a step, a ball mill finishes over it (taking the stock left by the first), a pass runs the
    /// holder into a wall and a plunge buries the shank.
    #[test]
    fn skipping_blocks_out_of_reach_changes_nothing() {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam::default();
        let mut flat = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && (t.diameter - 6.0).abs() < 0.5).unwrap();
        flat.stickout = 20.0;
        let ball = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Ball).unwrap();
        let (tf, tb) = (cam.add_tool(flat), cam.add_tool(ball));
        let stock = (DVec3::new(0.0, 0.0, 0.0), DVec3::new(60.0, 40.0, 30.0));
        let mut rough = vec![Move::Rapid([-5.0, 3.0, 35.0])];
        for (k, z) in [27.0, 24.0, 21.0].into_iter().enumerate() {
            for j in 0..12 {
                let y = 3.0 + j as f64 * 3.0;
                let (a, b) = if j % 2 == 0 { (-5.0, 40.0 + 2.0 * k as f64) } else { (40.0 + 2.0 * k as f64, -5.0) };
                rough.push(Move::Feed([a, y, z], 1200.0));
                rough.push(Move::Feed([b, y, z], 1200.0));
            }
        }
        // the holder over the wall the roughing left, then the shank down into the stock beside it
        rough.extend([Move::Rapid([-5.0, 20.0, 40.0]), Move::Feed([55.0, 20.0, 12.0], 800.0), Move::Rapid([55.0, 20.0, 40.0]), Move::Rapid([30.0, 30.0, 40.0]), Move::Feed([30.0, 30.0, 0.5], 300.0)]);
        let mut finish = vec![Move::Rapid([0.0, 0.0, 35.0])];
        for j in 0..40 {
            let y = j as f64;
            let z = |x: f64| 20.0 + 0.15 * x + 0.5 * (y * 0.3).sin();
            let xs: Vec<f64> = (0..=30).map(|i| i as f64 * 2.0).collect();
            for x in if j % 2 == 0 { xs.clone() } else { xs.iter().rev().copied().collect() } {
                finish.push(Move::Feed([x, y, z(x)], 1500.0));
            }
        }
        let paths = [Toolpath { op: 1, tool: tf, moves: rough, ..Default::default() }, Toolpath { op: 2, tool: tb, moves: finish, ..Default::default() }];
        let run = |off: bool| {
            let mut sim = Sim::new(&cam, &part, stock, &paths, 400);
            sim.bounds.off = off;
            sim.removed_by_move = Some(vec![(0.0, 0.0); sim.total()]);
            // in pieces, as the editor runs it
            while !sim.run_to(usize::MAX, 500) {}
            sim
        };
        let (a, b) = (run(false), run(true));
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&a.stock), bits(&b.stock));
        assert_eq!(bits(&a.lo), bits(&b.lo));
        assert_eq!(a.removed.to_bits(), b.removed.to_bits());
        assert_eq!(format!("{:?}", a.events), format!("{:?}", b.events));
        assert_eq!(format!("{:?}", a.removed_by_move), format!("{:?}", b.removed_by_move));
        assert_eq!(serde_json::to_string(&a.loads).unwrap(), serde_json::to_string(&b.loads).unwrap());
        assert!(a.events.iter().any(|e| e.kind == EventKind::Holder) && a.events.iter().any(|e| e.kind == EventKind::Shank), "{:?}", a.events);
        assert!(a.removed > 1000.0);
        // and the bounds stay above the stock in every block
        let mut s = a.clone();
        let (nx, ny) = (s.nx, s.ny);
        for bj in 0..s.bounds.ny {
            for bi in 0..s.bounds.nx {
                let before = s.bounds.top[bj * s.bounds.nx + bi];
                let exact = s.bounds.top(bi, bj, &a.stock, nx, ny);
                assert!(before >= exact);
            }
        }
        s.bounds.refresh(&a.stock, nx, ny);
        assert!(a.floor <= a.lo.iter().copied().fold(f32::INFINITY, f32::min));
    }

    /// A drill pecking down through a plate: every re-entry rapid stops 0.3 mm above the last depth, so it
    /// must never cut. The hole sits a hair off the grid (as a hole found on a real model does), which puts
    /// cells right on the drill's rim: the plunge must sweep the same footprint all the way down.
    #[test]
    fn peck_drill_reentry_rapids_cut_nothing() {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam::default();
        let drill = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Drill && (t.diameter - 6.8).abs() < 1e-9).unwrap();
        let t = cam.add_tool(drill);
        let stock = (DVec3::new(-10.0, -10.0, 0.0), DVec3::new(90.0, 60.0, 13.0));
        for y in [15.0, 15.000000000000002, 14.999999999999998, 35.000000000000007] {
            let (x, z) = (60.0, |d: f64| 13.0 + d);
            let at = |d: f64| [x, y, z(d)];
            let moves = vec![
                Move::Rapid(at(10.0)),
                Move::Rapid(at(1.0)),
                Move::Feed(at(-7.8), 300.0),
                Move::Rapid(at(1.0)),
                Move::Rapid(at(-7.5)),
                Move::Feed(at(-14.6), 300.0),
                Move::Rapid(at(1.0)),
                Move::Rapid(at(-14.3)),
                Move::Feed(at(-15.543), 300.0),
                Move::Rapid(at(10.0)),
            ];
            let tp = Toolpath { op: 1, tool: t, moves, ..Default::default() };
            let mut sim = Sim::new(&cam, &part, stock, &[tp], 500);
            assert!((sim.cell - 0.2).abs() < 1e-12);
            sim.run_to(usize::MAX, usize::MAX);
            assert!(sim.events.is_empty(), "hole at y = {y:?}: {:?}", sim.events);
            assert!(sim.removed > 0.0);
        }
    }

    /// A slot with a feed move above the stock (air, at a link feed over max_feed), a plunge, a dwell and
    /// rapids: the split adds up to the cycle time with each part where it belongs.
    #[test]
    fn time_split_counts_air_rapids_and_between_operations() {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam { rapid_rate: 6000.0, ..Default::default() };
        cam.machine.rapid_z = 0.0;
        cam.machine.spin_up = 5.0;
        cam.machine.max_feed = 1000.0;
        let flat = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap();
        let t = cam.add_tool(flat);
        let stock = (DVec3::new(-10.0, -10.0, 0.0), DVec3::new(90.0, 60.0, 10.0));
        let moves = vec![
            Move::Rapid([0.0, 20.0, 15.0]),
            // 60 mm in air above the stock at 1200 mm/min: 3 s, a link
            Move::Feed([60.0, 20.0, 15.0], 1200.0),
            // 5 mm at 6000 mm/min: 0.05 s
            Move::Rapid([60.0, 20.0, 10.0]),
            // plunge 1 mm at 300: 0.2 s; slot 40 mm at 600: 4 s
            Move::Feed([60.0, 20.0, 9.0], 300.0),
            Move::Feed([20.0, 20.0, 9.0], 600.0),
            Move::Dwell(2.0),
            // 0.06 s
            Move::Rapid([20.0, 20.0, 15.0]),
        ];
        let mut tp = Toolpath { op: 1, tool: t, moves, ..Default::default() };
        let tool = cam.tool(t).unwrap().clone();
        let m = crate::cam_paths::move_times(&tp, &tool, &cam);
        assert!((m.feed_s - 7.2).abs() < 1e-9 && (m.rapid_s - 0.11).abs() < 1e-9 && m.dwell_s == 2.0, "{m:?}");
        tp.time = m.total();
        let paths = [tp];
        let mut sim = Sim::new(&cam, &part, stock, &paths, 500);
        sim.run_to(usize::MAX, usize::MAX);
        // (the preset's default feed overloads it in a full slot: not what this is about)
        assert!(sim.events.iter().all(|e| e.kind == EventKind::Overload), "{:?}", sim.events);
        let a = sim.air[0];
        assert!((a.feed_s - 7.2).abs() < 1e-6, "{a:?}");
        assert!((a.air_s - 3.0).abs() < 0.1 && (a.link_s - 3.0).abs() < 1e-6, "{a:?}");
        let total = crate::cam_paths::cycle_time(&cam, &paths);
        assert!((total - 14.31).abs() < 1e-9, "{total}");
        let s = time_split(&cam, &paths, &sim.air, total);
        let close = |x: f64, y: f64, tol: f64| assert!((x - y).abs() < tol, "{x} vs {y}: {s:?}");
        close(s.air_s, 3.0, 0.1);
        close(s.link_s, 3.0, 1e-6);
        close(s.cut_s, 4.2, 0.1);
        close(s.rapid_s, 0.11, 1e-9);
        close(s.other_s, 2.0, 1e-9);
        close(s.between_s, 5.0, 1e-9);
        close(s.cut_s + s.air_s + s.rapid_s + s.other_s + s.between_s, total, 1e-9);
        close(s.pct(s.air_s), 100.0 * 3.0 / 14.31, 0.7);
        close(s.pct(s.non_cutting_s()), 100.0 * 8.11 / 14.31, 0.7);
        let j = s.json();
        close(j["air_cut_pct"].as_f64().unwrap(), 21.0, 0.7);
        // with no simulation every feed second counts as cutting
        let none = time_split(&cam, &paths, &[], total);
        close(none.air_s, 0.0, 1e-12);
        close(none.cut_s, 7.2, 1e-9);
    }

    /// A 2.24° tapered ball (R1, as the SpeTool W01004) running along a wall that stands 8 mm above its tip,
    /// just clear of the wall's top edge: the flank touches nothing, so no gouge. Reading the tool's height at
    /// the table step below the cell's distance made the steep flank up to 0.05 mm too low and flagged it.
    /// 0.05 mm closer it really cuts the wall and must be flagged.
    #[test]
    fn tapered_flank_beside_a_tall_wall_is_not_a_gouge() {
        let v = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        let q = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| [[a, b, c], [a, c, d]];
        let (w, l, h) = (20.0, 20.0, 10.0);
        let mut t = vec![];
        t.extend(q(v(0., 0., h), v(w, 0., h), v(w, l, h), v(0., l, h)));
        t.extend(q(v(0., 0., 0.), v(0., l, 0.), v(w, l, 0.), v(w, 0., 0.)));
        t.extend(q(v(0., 0., 0.), v(w, 0., 0.), v(w, 0., h), v(0., 0., h)));
        t.extend(q(v(w, 0., 0.), v(w, l, 0.), v(w, l, h), v(w, 0., h)));
        t.extend(q(v(w, l, 0.), v(0., l, 0.), v(0., l, h), v(w, l, h)));
        t.extend(q(v(0., l, 0.), v(0., 0., 0.), v(0., 0., h), v(0., l, h)));
        let part = Part::new(&t, &[], "top");
        let mut cam = Cam::default();
        let tool = crate::cam::Tool { kind: ToolKind::TaperedBall, diameter: 3.175, corner_radius: 1.0, angle: 2.24, flute_length: 15.0,
                                      shank_diameter: 3.175, stickout: 18.0, ..Default::default() };
        let ti = cam.add_tool(tool.clone());
        // the flank's radius at the wall's top edge (about 8 above the tip); the tip height is picked so that
        // radius sits just short of one of the simulation's table steps
        let reach_at = |tip_z: f64| {
            let (mut lo, mut hi) = (0.9, 1.5875);
            for _ in 0..60 {
                let m = (lo + hi) / 2.0;
                if crate::cam_3d::tool_height(&tool, m).unwrap() < h - tip_z { lo = m } else { hi = m }
            }
            hi
        };
        let tip_z = (0..200).map(|k| 2.0 + k as f64 * 0.0005).find(|&z| ((reach_at(z) / LUT_STEP).fract() - 0.88).abs() < 0.02).unwrap();
        let reach = reach_at(tip_z);
        assert!(reach > 1.25 && reach < 1.3, "{reach}");
        let stock = (DVec3::new(0.0, -5.0, 0.0), DVec3::new(40.0, l + 5.0, h));
        // clear of the edge by 0.0002: the step below is about 0.0018 mm nearer the axis, where the flank is
        // 0.045 mm lower
        let clear = 0.0002;
        for (gap, real) in [(clear, false), (-0.05, true)] {
            let x = w + reach + gap;
            let moves = vec![Move::Rapid([x, -3.0, 20.0]), Move::Rapid([x, -3.0, tip_z]), Move::Feed([x, l + 3.0, tip_z], 1000.0), Move::Rapid([x, l + 3.0, 20.0])];
            let tp = Toolpath { op: 1, tool: ti, moves, ..Default::default() };
            let mut sim = Sim::new(&cam, &part, stock, &[tp], 1000);
            assert!((sim.cell - 0.04).abs() < 1e-12);
            sim.run_to(usize::MAX, usize::MAX);
            let gouges: Vec<_> = sim.events.iter().filter(|e| e.kind == EventKind::Gouge).collect();
            assert_eq!(!gouges.is_empty(), real, "gap {gap}: {gouges:?}");
        }
    }
}
