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

struct Cutter {
    radius: f64,
    /// Height of the cutting surface above the tip by distance from the axis, every LUT_STEP mm.
    lut: Vec<f32>,
    /// Shank radius and bottom height above the tip.
    shank_r: f64,
    shank_z: f32,
    /// Cells under the holder (every other cell) and how far above the tip its underside is there.
    holder: Vec<(i32, i32, f32)>,
    /// Cuts under an overhang (slot mills, thread mills): not modelled in the height map.
    undercut: bool,
    /// How deep it may cut into the part without it counting (chamfers, taps, threads).
    allowance: f32,
    /// The tool and holder as bands from the tip up: (radius, height of the band's bottom above the tip).
    body: Vec<(f64, f64)>,
    /// Length out of the holder, and the holder's length up to the spindle (for placing it in the machine).
    stickout: f64,
    holder_len: f64,
}

pub const LUT_STEP: f64 = 0.002;

/// Height of a tool's cutting surface above its tip, every LUT_STEP mm out from the axis
/// (empty for tools that cut under an overhang).
pub fn tool_lut(t: &Tool) -> Vec<f32> {
    if matches!(t.kind, ToolKind::Slot | ToolKind::ThreadMill) {
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
    let undercut = matches!(t.kind, ToolKind::Slot | ToolKind::ThreadMill);
    let flutes = if undercut { t.cutting_width.max(0.5) } else { t.flute_length.max(r) };
    let neck = if undercut {
        if t.neck_diameter > 0.0 {
            t.neck_diameter / 2.0
        } else {
            r * 0.6
        }
    } else {
        (t.shank_diameter / 2.0).max(0.1)
    };
    let stick = t.stickout.max(flutes + 1.0);
    let mut body = vec![(r, 0.0), (neck, flutes)];
    body.extend(t.holder_outline().iter().map(|(hr, h)| (*hr, stick + h)));
    body
}

impl Cutter {
    fn new(t: &Tool, cell: f64, allowance: f32) -> Cutter {
        let r = t.radius();
        let undercut = matches!(t.kind, ToolKind::Slot | ToolKind::ThreadMill);
        let lut = tool_lut(t);
        let flutes = if undercut { t.cutting_width.max(0.5) } else { t.flute_length.max(r) };
        let neck = if undercut {
            if t.neck_diameter > 0.0 {
                t.neck_diameter / 2.0
            } else {
                r * 0.6
            }
        } else {
            (t.shank_diameter / 2.0).max(0.1)
        };
        let stick = t.stickout.max(flutes + 1.0);
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
        Cutter { radius: r, lut, shank_r: neck, shank_z: flutes as f32, holder, undercut, allowance, body, stickout: stick, holder_len: crate::cam_machine::holder_length(t) }
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

pub struct Sim {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    /// Top of the material over each cell (EMPTY once cut through) and its bottom.
    pub stock: Vec<f32>,
    pub lo: Vec<f32>,
    pub part: Vec<f32>,
    pub gouged: Vec<bool>,
    /// The last few toolpaths that cut each cell, newest first: (index + 1, 0 = none; the height the
    /// material was at when it started cutting there). Colours the walls by who exposed them.
    pub cuts: Vec<[(u8, f32); 3]>,
    pub top: f32,
    pub bottom: f32,
    moves: Vec<(usize, u64, Move)>,
    cutters: Vec<Cutter>,
    /// Next move to run.
    pub done: usize,
    pos: DVec3,
    pub events: Vec<SimEvent>,
    pub removed: f64,
    max_h: f32,
    /// Fixtures (setup coordinates).
    fixtures: Vec<(DVec3, DVec3)>,
    /// The highest fixture top: the tool's tip above it can't touch them.
    fixture_top: f64,
    /// The machine, when the setup has one.
    pub machine: Option<SimMachine>,
}

/// The machine model placed for the simulation, and each toolpath's tool as points for checking it.
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
            fixture_top: f64::INFINITY,
            machine: None,
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
        for tp in paths {
            s.push(cam, tp);
        }
        s
    }

    /// Queue another toolpath to run after the ones already given.
    pub fn push(&mut self, cam: &Cam, tp: &Toolpath) {
        // every toolpath gets a cutter, so cutter indices match toolpath indices
        let Some(tool) = cam.tool(tp.tool) else {
            self.cutters.push(Cutter::new(&Tool::default(), self.cell, 0.0));
            return;
        };
        let op = cam.op(tp.op);
        let allowance = match op.map(|o| o.kind) {
            Some(OpKind::Chamfer) | Some(OpKind::Deburr) => op.map(|o| o.chamfer * 2.0 + 0.3).unwrap_or(1.0) as f32,
            Some(OpKind::Tap) | Some(OpKind::Thread) => f32::INFINITY,
            // engraving cuts into the part on purpose, as deep as asked
            Some(OpKind::Trace) => op.map(|o| o.depth + 0.05).unwrap_or(1.0) as f32,
            Some(OpKind::VCarve) => op.map(|o| if o.depth > 0.0 { o.depth + 0.05 } else { tool.flute_length }).unwrap_or(1.0) as f32,
            _ => 0.02,
        };
        self.cutters.push(Cutter::new(tool, self.cell, allowance));
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
    }

    /// The stock as it is now.
    pub fn stock_map(&self) -> StockMap {
        StockMap { x0: self.x0, y0: self.y0, cell: self.cell, nx: self.nx, ny: self.ny, hi: self.stock.clone(), lo: self.lo.clone() }
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
        let mut spent = 0usize;
        while self.done < target {
            let (ci, op, m) = self.moves[self.done];
            let step = self.done;
            match m {
                Move::Rapid(p) => spent += self.travel(ci, op, step, DVec3::from(p), true),
                Move::Feed(p, _) => spent += self.travel(ci, op, step, DVec3::from(p), false),
                Move::Tap { z, r, .. } => {
                    let a = self.pos;
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, z), false);
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, r), false);
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
            // nothing to touch above the stock
            if p.z as f32 > self.max_h + 1e-3 {
                continue;
            }
            work += 1;
            // straight up only ever leaves material: it can't be a rapid into the stock (the tool can sit beside a
            // wall the grid shows a little inside it)
            let rising = to.z > from.z && (to - from).truncate().length() < 1e-9;
            self.stamp(ci, op, step, p, rapid && !rising, k % check_every == 0 || k == n);
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

    fn stamp(&mut self, ci: usize, op: u64, step: usize, p: DVec3, rapid: bool, check_holder: bool) {
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
            let inner = (c.radius - 0.02).max(0.0);
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    let d2 = dx * dx + dy * dy;
                    if d2 > r2 {
                        continue;
                    }
                    let rho = d2.sqrt();
                    let z = tip + c.lut[((rho / LUT_STEP) as usize).min(c.lut.len() - 1)];
                    let k = (j * nx + i) as usize;
                    let s = self.stock[k];
                    if s > z {
                        let floor = self.lo[k];
                        let cut = s - z.max(floor);
                        removed += cut * area;
                        deepest = deepest.max(cut);
                        // cut right through the column: nothing left over this cell
                        self.stock[k] = if z <= floor { EMPTY } else { z };
                        let tag = (ci + 1).min(250) as u8;
                        let h = &mut self.cuts[k];
                        if h[0].0 != tag {
                            h.rotate_right(1);
                            h[0] = (tag, s);
                        }
                        if rho <= inner && self.part[k] > z + c.allowance {
                            // depth across the surface, not straight down: on a steep wall a hair's
                            // breadth sideways reads as a deep cut from above
                            let d = (self.part[k] - z) / self.slope_factor(i, j);
                            if d > c.allowance {
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
        if check_holder {
            let sz = tip + c.shank_z;
            // cells clearly inside the shank (it may rub a wall its own flutes cut)
            let rs = c.shank_r - 0.02;
            let n = (rs / self.cell).ceil() as i32;
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
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
            for &(di, dj, dz) in &c.holder {
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
    }

    /// Triangles of the simulated stock (setup coordinates), at most `max_cells` along each side:
    /// positions and per-triangle colour class: CLASS_STOCK (untouched), CLASS_GOUGE, CLASS_EARLIER
    /// (machined in an earlier setup) or CLASS_PATH + the index of the toolpath that cut it last.
    pub fn mesh(&self, max_cells: usize) -> (Vec<[DVec3; 3]>, Vec<u8>) {
        let k = ((self.nx.max(self.ny) as f64 / max_cells.max(10) as f64).ceil() as usize).max(1);
        let (mx, my) = (self.nx.div_ceil(k), self.ny.div_ceil(k));
        let bz = self.bottom as f64;
        // each block's column of material [lo, hi] (hi <= lo: none) and colour
        let mut h = vec![0.0f64; mx * my];
        let mut l = vec![0.0f64; mx * my];
        let mut cls = vec![0u8; mx * my];
        let mut cuts = vec![[(0u8, 0.0f32); 3]; mx * my];
        for bj in 0..my {
            for bi in 0..mx {
                let mut top = f32::INFINITY;
                let mut low = self.bottom;
                let mut g = false;
                let mut by = [(0u8, 0.0f32); 3];
                for j in bj * k..((bj + 1) * k).min(self.ny) {
                    for i in bi * k..((bi + 1) * k).min(self.nx) {
                        let q = j * self.nx + i;
                        if self.stock[q] < top {
                            top = self.stock[q];
                            by = self.cuts[q];
                            low = self.lo[q];
                        }
                        g |= self.gouged[q];
                    }
                }
                let b = bj * mx + bi;
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
}
