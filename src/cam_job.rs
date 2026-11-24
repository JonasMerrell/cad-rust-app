//! Jobs: several setups machined in one program, each at its own station on the machine (e.g. the two
//! stations of a vise: op 1 of a raw blank in one, op 2 of the blank flipped from the last cycle in the other).
//!
//! A setup lists the others it runs with (`Cam::with`); it leads the job. Each station keeps everything of its
//! own setup (part, stock, the setup it follows, fixtures, work offset, `on_table`). The job runs the stations'
//! toolpaths in `job_order` (each station in its own order, each tool once per run of it) and posts them as one
//! program. For the simulation, every station is moved into the lead station's setup coordinates by where its
//! `on_table` puts it, so one stock map and one machine check cover them all; operations and tools get ids of
//! their own there (`station * ID_STRIDE + id`).

use glam::{DMat3, DVec3};

use crate::cam::{Cam, Fixture, Origin};
use crate::cam_paths::{self, Move, Part, Toolpath};
use crate::cam_sim::Sim;
use crate::cam_stock::{StockMap, EMPTY};

/// Operation and tool ids of station `s` in the combined setup start at `s * ID_STRIDE`.
pub const ID_STRIDE: u64 = 1_000_000;

/// One station, built: its setup, part, toolpaths and the stock its setup starts from.
pub struct JobStation {
    pub cam: Cam,
    pub part: Part,
    pub paths: Vec<Toolpath>,
    /// The stock an earlier setup left (None: the setup's own stock box, solid).
    pub left: Option<StockMap>,
    /// Where this station's setup coordinates sit in the lead station's.
    pub offset: DVec3,
}

impl JobStation {
    pub fn new(cam: Cam, part: Part, paths: Vec<Toolpath>, left: Option<StockMap>) -> JobStation {
        JobStation { cam, part, paths, left, offset: DVec3::ZERO }
    }

    pub fn stock(&self) -> (DVec3, DVec3) {
        cam_paths::stock_box(&self.cam, &self.part)
    }

    pub fn origin(&self) -> DVec3 {
        cam_paths::work_origin(&self.cam, &self.part)
    }

    /// The setup point that sits at the setup's `on_table` (see `Placed::reference`).
    fn reference(&self) -> DVec3 {
        let fixtures = cam_paths::fixture_boxes(&self.cam, &self.part, self.origin());
        crate::cam_machine::Placed::reference(self.stock(), &fixtures)
    }
}

pub struct Job {
    pub stations: Vec<JobStation>,
    pub order: Vec<(usize, usize)>,
}

/// What `check` finds wrong with a job.
pub fn check(stations: &[&Cam]) -> Vec<String> {
    let mut w = vec![];
    for (i, a) in stations.iter().enumerate() {
        for b in &stations[i + 1..] {
            // the same work offset is right for setups at the same station (an engraving after op 2)
            if a.wcs() == b.wcs() && a.on_table != b.on_table {
                w.push(format!("{} and {} both use G{}: give each station its own work_offset", a.name, b.name, 53 + a.wcs() as u32));
            }
            if a.post != b.post {
                w.push(format!("{} posts for {} and {} for {}", a.name, a.post, b.name, b.post));
            }
            if a.machine.model != b.machine.model {
                w.push(format!("{} and {} are on different machine models ({} / {}): the job uses {}", a.name, b.name, a.machine.model, b.machine.model, stations[0].machine.model));
            }
        }
    }
    w
}

impl Job {
    /// The stations placed relative to the first (the lead) and their toolpaths in job order.
    pub fn new(mut stations: Vec<JobStation>) -> Job {
        let lead = stations[0].reference() - DVec3::from(stations[0].cam.on_table);
        for s in stations.iter_mut() {
            s.offset = lead - (s.reference() - DVec3::from(s.cam.on_table));
        }
        let tools: Vec<Vec<Option<u32>>> = stations
            .iter()
            .map(|s| s.paths.iter().map(|p| if p.moves.is_empty() { None } else { s.cam.tool(p.tool).map(|t| t.number) }).collect())
            .collect();
        let order = crate::cam_post::job_order(&tools);
        Job { stations, order }
    }

    /// The order with only some stations (e.g. op 1 alone for the first cycle).
    pub fn only(&mut self, keep: &[usize]) {
        self.order.retain(|(s, _)| keep.contains(s));
    }

    /// Tool changes in the job's order.
    pub fn tool_changes(&self) -> usize {
        let mut n = 0;
        let mut cur = None;
        for &(s, p) in &self.order {
            let st = &self.stations[s];
            let t = st.cam.tool(st.paths[p].tool).map(|t| t.number);
            if t != cur {
                n += 1;
                cur = t;
            }
        }
        n
    }

    pub fn cycle_time(&self) -> f64 {
        self.order.iter().map(|&(s, p)| self.stations[s].paths[p].time).sum()
    }

    /// The one program for the job.
    pub fn gcode(&self) -> String {
        let st: Vec<crate::cam_post::Station> = self
            .stations
            .iter()
            .map(|s| {
                let ctx = cam_paths::Ctx::new(&s.cam, &s.part);
                let stock = cam_paths::tilted_block(&s.cam, &s.part).is_none().then_some(ctx.stock);
                crate::cam_post::Station { cam: &s.cam, paths: &s.paths, origin: ctx.origin, safe: ctx.safe(), work_zero: Some(cam_paths::work_zero_note(&s.cam, &s.part)), stock }
            })
            .collect();
        crate::cam_post::gcode_job(&st, Some(self.order.clone()))
    }

    /// The station and own operation id of a combined operation id.
    pub fn station_of(&self, op: u64) -> (usize, u64) {
        ((op / ID_STRIDE) as usize, op % ID_STRIDE)
    }

    /// A point in the combined setup as the work coordinates of station `s`.
    pub fn work_point(&self, s: usize, p: DVec3) -> DVec3 {
        let st = &self.stations[s];
        p - st.offset - st.origin()
    }

    /// Everything as one setup in the lead station's coordinates: the setup (tools and operations of all
    /// stations, renumbered and named after their station; the fixtures of all; work zero at the lead's setup
    /// origin), the parts, the toolpaths in job order with a move over the top between stations, the stock box
    /// round them all and the stock they start from.
    pub fn combined(&self) -> (Cam, Part, Vec<Toolpath>, (DVec3, DVec3)) {
        let lead = &self.stations[0];
        let mut c = lead.cam.clone();
        c.name = self.stations.iter().map(|s| s.cam.name.clone()).collect::<Vec<_>>().join(" + ");
        c.with.clear();
        c.origin = Origin::Model;
        c.tools.clear();
        c.ops.clear();
        c.fixtures.clear();
        c.soft_jaws = None;
        c.follows.clear();
        let mut tris = vec![];
        let mut circles = vec![];
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for (i, s) in self.stations.iter().enumerate() {
            let base = i as u64 * ID_STRIDE;
            for t in &s.cam.tools {
                let mut t = t.clone();
                t.id += base;
                c.tools.push(t);
            }
            for o in &s.cam.ops {
                let mut o = o.clone();
                o.id += base;
                o.tool += base;
                if self.stations.len() > 1 {
                    o.name = format!("{}: {}", s.cam.name, o.name);
                }
                c.ops.push(o);
            }
            let so = s.origin();
            for (name, a, b) in cam_paths::fixture_boxes(&s.cam, &s.part, so) {
                let (a, b) = (a + s.offset, b + s.offset);
                let name = if self.stations.len() > 1 { format!("{}: {name}", s.cam.name) } else { name };
                c.fixtures.push(Fixture { name, min: a.to_array(), max: b.to_array() });
            }
            tris.extend(s.part.tris.iter().map(|t| t.map(|p| p + s.offset)));
            circles.extend(s.part.circles.iter().map(|(a, b, r)| (*a + s.offset, *b + s.offset, *r)));
            let (a, b) = s.stock();
            lo = lo.min(a + s.offset);
            hi = hi.max(b + s.offset);
        }
        let mut plo = DVec3::splat(f64::INFINITY);
        let mut phi = DVec3::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                plo = plo.min(*p);
                phi = phi.max(*p);
            }
        }
        let part = Part { tris, circles, lo: plo, hi: phi, rot: DMat3::IDENTITY, sketches: vec![] };
        c.stock.carried = Some([lo.to_array(), hi.to_array()]);
        c.job_vise = self.stations.iter().flat_map(|s| s.cam.vise_closings()).collect();
        c.job_notes = self
            .stations
            .iter()
            .enumerate()
            .map(|(i, s)| format!("station {}: {} in G{} (work zero: {})", i + 1, s.cam.name, 53 + s.cam.wcs() as u32, cam_paths::work_zero_note(&s.cam, &s.part)))
            .collect();

        let items: Vec<(usize, Toolpath)> = self.order.iter().map(|&(si, pi)| (si, self.stations[si].paths[pi].clone())).collect();
        let paths = self.place(items);
        (c, part, paths, (lo, hi))
    }

    /// Toolpaths of the stations (in their own setup coordinates and ids), in the order given, as the combined
    /// setup runs them: renumbered, moved into the lead's coordinates, and up to the highest station's safe
    /// height and over between stations.
    pub fn place(&self, items: Vec<(usize, Toolpath)>) -> Vec<Toolpath> {
        let top = self.stations.iter().map(|s| cam_paths::Ctx::new(&s.cam, &s.part).safe() + s.offset.z).fold(f64::NEG_INFINITY, f64::max);
        let mut paths = vec![];
        let mut last: Option<(usize, DVec3)> = None;
        for (si, mut tp) in items {
            let s = &self.stations[si];
            let base = si as u64 * ID_STRIDE;
            tp.op += base;
            tp.tool += base;
            let shift = |p: [f64; 3]| (DVec3::from(p) + s.offset).to_array();
            for m in tp.moves.iter_mut() {
                *m = match *m {
                    Move::Rapid(p) => Move::Rapid(shift(p)),
                    Move::Feed(p, f) => Move::Feed(shift(p), f),
                    Move::Tap { z, r, pitch } => Move::Tap { z: z + s.offset.z, r: r + s.offset.z, pitch },
                    Move::Dwell(d) => Move::Dwell(d),
                };
            }
            if let (Some((ls, lp)), Some(first)) = (last, tp.moves.iter().find_map(|m| match m {
                Move::Rapid(p) | Move::Feed(p, _) => Some(DVec3::from(*p)),
                _ => None,
            })) {
                if ls != si {
                    tp.moves.insert(0, Move::Rapid([first.x, first.y, top]));
                    tp.moves.insert(0, Move::Rapid([lp.x, lp.y, top]));
                }
            }
            let mut end = last.map(|l| l.1).unwrap_or(DVec3::ZERO);
            for m in &tp.moves {
                match m {
                    Move::Rapid(p) | Move::Feed(p, _) => end = DVec3::from(*p),
                    Move::Tap { r, .. } => end.z = *r,
                    Move::Dwell(_) => {}
                }
            }
            if !tp.moves.is_empty() {
                last = Some((si, end));
            }
            paths.push(tp);
        }
        paths
    }

    /// The stock the job starts from on a grid (the simulation's, so nothing is resampled twice): each
    /// station's, as its setup starts it, and nothing between them.
    pub fn stock_on(&self, x0: f64, y0: f64, cell: f64, nx: usize, ny: usize, floor: f64) -> StockMap {
        let mut map = StockMap { x0, y0, cell, nx, ny, hi: vec![EMPTY; nx * ny], lo: vec![floor as f32; nx * ny] };
        for j in 0..ny {
            let y = y0 + j as f64 * cell;
            for i in 0..nx {
                let x = x0 + i as f64 * cell;
                let k = j * nx + i;
                for s in &self.stations {
                    let (a, b) = s.stock();
                    let (px, py) = (x - s.offset.x, y - s.offset.y);
                    let col = match &s.left {
                        Some(m) => m.column(px, py),
                        None => (px >= a.x && px <= b.x && py >= a.y && py <= b.y).then_some((a.z as f32, b.z as f32)),
                    };
                    if let Some((l, h)) = col {
                        map.lo[k] = l + s.offset.z as f32;
                        map.hi[k] = h + s.offset.z as f32;
                        break;
                    }
                }
            }
        }
        map
    }

    /// The combined simulation, not yet run, with the lead's machine (if it has one).
    pub fn sim(&self, resolution: usize) -> Result<(Cam, Part, Vec<Toolpath>, Sim), String> {
        let (c, part, paths, stock) = self.combined();
        self.sim_of(c, part, paths, stock, resolution)
    }

    /// The combined simulation of other toolpaths (`place`d), e.g. read back from the posted program.
    pub fn sim_with(&self, paths: Vec<Toolpath>, resolution: usize) -> Result<(Cam, Part, Vec<Toolpath>, Sim), String> {
        let (c, part, _, stock) = self.combined();
        self.sim_of(c, part, paths, stock, resolution)
    }

    /// Split a job's program into its stations' stretches: the first station's from its work-offset line (G54…G59),
    /// each later one from the post's "(station N: name)" switch comment, each starting with the tool that is in
    /// at that point, for reading back one station at a time.
    pub fn split_program(&self, text: &str) -> Vec<(usize, String)> {
        let first = self.order.first().map(|o| o.0).unwrap_or(0);
        let switch = |l: &str| {
            let c = l.strip_prefix("(station ")?.strip_suffix(')')?;
            let (n, name) = c.split_once(": ")?;
            let i = n.trim().parse::<usize>().ok()?.checked_sub(1)?;
            (self.stations.get(i)?.cam.name == name).then_some(i)
        };
        let mut out: Vec<(usize, String)> = vec![];
        let mut cur: Option<usize> = None;
        let mut tool: Option<String> = None;
        let mut buf = String::new();
        for line in text.lines() {
            let l = line.trim();
            let start = match cur {
                None => (l == format!("G{}", 53 + self.stations[first].cam.wcs() as u32)).then_some(first),
                Some(_) => switch(l),
            };
            if let Some(si) = start {
                if let Some(c) = cur {
                    out.push((c, std::mem::take(&mut buf)));
                }
                if let Some(t) = &tool {
                    buf.push_str(t);
                    buf.push('\n');
                }
                cur = Some(si);
            }
            if l.starts_with('T') && l.ends_with("M6") {
                tool = Some(l.to_string());
            }
            if cur.is_some() {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        if let Some(c) = cur {
            out.push((c, buf));
        }
        out
    }

    fn sim_of(&self, c: Cam, part: Part, paths: Vec<Toolpath>, stock: (DVec3, DVec3), resolution: usize) -> Result<(Cam, Part, Vec<Toolpath>, Sim), String> {
        let mut sim = Sim::new(&c, &part, stock, &paths, resolution);
        let map = self.stock_on(sim.x0, sim.y0, sim.cell, sim.nx, sim.ny, stock.0.z);
        sim.start_from(&map);
        if !c.machine.model.is_empty() {
            let l = crate::cam_vise::machine_for(&c)?;
            let lead = &self.stations[0];
            let placed = crate::cam_machine::Placed::new(l, lead.reference(), DVec3::from(lead.cam.on_table));
            sim.set_machine(&c, &part, std::sync::Arc::new(placed));
        }
        crate::cam_sim_saves::keep_ops(&mut sim, &c, &paths, 0);
        Ok((c, part, paths, sim))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::{OpKind, ToolKind};

    fn station(name: &str, wcs: u8, x: f64, tools: &[(u32, OpKind)]) -> JobStation {
        let mut c = Cam { name: name.into(), work_offset: wcs, post: "rcnc".into(), on_table: [x, 0.0, 0.0], ..Default::default() };
        c.stock.size = Some([30.0, 30.0, 10.0]);
        let mut paths = vec![];
        for (n, kind) in tools {
            let mut t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap();
            t.number = *n;
            let tid = match c.tools.iter().find(|x| x.number == *n) {
                Some(x) => x.id,
                None => c.add_tool(t),
            };
            let op = c.add_op(*kind, tid);
            paths.push(Toolpath { op, tool: tid, moves: vec![Move::Rapid([0.0, 0.0, 15.0]), Move::Rapid([0.0, 0.0, 9.0]), Move::Feed([10.0, 0.0, 9.0], 600.0), Move::Rapid([10.0, 0.0, 15.0])], time: 2.0, ..Default::default() });
        }
        let s = 15.0;
        let tris = vec![[DVec3::new(-s, -s, 0.0), DVec3::new(s, -s, 0.0), DVec3::new(s, s, 0.0)], [DVec3::new(-s, -s, 0.0), DVec3::new(s, s, 0.0), DVec3::new(-s, s, 0.0)]];
        let part = Part::new(&tris, &[], "top");
        JobStation::new(c, part, paths, None)
    }

    #[test]
    fn job_order_groups_tools_and_keeps_each_stations_order() {
        // station A: probe, rough, chamfer; station B: probe, rough, finish with the rougher, chamfer
        let order = crate::cam_post::job_order(&[vec![Some(99), Some(24), Some(1)], vec![Some(99), Some(24), Some(24), Some(1)]]);
        assert_eq!(order, vec![(0, 0), (1, 0), (0, 1), (1, 1), (1, 2), (0, 2), (1, 3)]);
        // a station that needs a tool back: it comes back later, never ahead of the station's own order
        let order = crate::cam_post::job_order(&[vec![Some(24), Some(1), Some(24)], vec![Some(1)]]);
        assert_eq!(order, vec![(0, 0), (0, 1), (1, 0), (0, 2)]);
        // empty toolpaths are skipped
        assert_eq!(crate::cam_post::job_order(&[vec![None, Some(5)]]), vec![(0, 1)]);
        // op 1 probe, end mill, chamfer mill; op 2 probe, end mill, ball, chamfer mill: the chamfer mill once,
        // after the ball (the first station's next tool first would load it twice)
        let order = crate::cam_post::job_order(&[vec![Some(99), Some(24), Some(1)], vec![Some(99), Some(24), Some(11), Some(1)]]);
        assert_eq!(order, vec![(0, 0), (1, 0), (0, 1), (1, 1), (1, 2), (0, 2), (1, 3)]);
    }

    #[test]
    fn two_stations_post_one_program_with_each_tool_once() {
        let a = station("Op1", 1, -50.0, &[(24, OpKind::Face), (1, OpKind::Chamfer)]);
        let b = station("Op2", 2, 50.0, &[(24, OpKind::Face), (1, OpKind::Chamfer)]);
        let job = Job::new(vec![a, b]);
        assert_eq!(job.tool_changes(), 2);
        // station 2 sits 100 mm along X from station 1
        assert!((job.stations[1].offset.x - 100.0).abs() < 1e-9, "{:?}", job.stations[1].offset);
        let g = job.gcode();
        assert_eq!(g.matches("T24 M6").count(), 1, "{g}");
        assert_eq!(g.matches("T1 M6").count(), 1, "{g}");
        // station 1's cut in G54, then up, G55, over at the top, station 2's cut, with the same tool
        let (g54, g55) = (g.find("\nG54\n").unwrap(), g.find("\nG55\n").unwrap());
        let (t24, t1) = (g.find("T24 M6").unwrap(), g.find("T1 M6").unwrap());
        assert!(g54 < t24 && t24 < g55 && g55 < t1, "{g}");
        let before = &g[..g55];
        assert!(before.trim_end().ends_with("G53 G0 Z0"), "goes to the top before switching: {g}");
        let first_move = g[g55..].lines().find(|l| l.starts_with("G0") || l.starts_with("G1")).unwrap();
        assert!(first_move.starts_with("G0 X") && !first_move.contains('Z'), "travels over at the top: {first_move}");
        // and back to G54 for station 1's chamfer, then G55 again for station 2's
        assert_eq!(g.matches("\nG54\n").count(), 2, "{g}");
        assert_eq!(g.matches("\nG55\n").count(), 2, "{g}");
        assert!(g.contains("station 1: G54 Op1") && g.contains("station 2: G55 Op2"), "{g}");
    }

    #[test]
    fn a_job_program_splits_back_into_its_stations() {
        let a = station("Op1", 1, -50.0, &[(24, OpKind::Face), (1, OpKind::Chamfer)]);
        let b = station("Op2", 2, 50.0, &[(24, OpKind::Face), (1, OpKind::Chamfer)]);
        let job = Job::new(vec![a, b]);
        let g = job.gcode();
        let parts = job.split_program(&g);
        assert_eq!(parts.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, 1, 0, 1], "{g}");
        // a later stretch starts with the tool that is in, and every move of the program lands in one stretch
        assert!(parts[1].1.starts_with("T24 M6\n") && parts[3].1.starts_with("T1 M6\n"), "{:?}", parts);
        let moves = |t: &str| t.lines().filter(|l| l.starts_with("G0") || l.starts_with("G1")).count();
        let first = g.find("\nG54\n").unwrap();
        assert_eq!(parts.iter().map(|p| moves(&p.1)).sum::<usize>(), moves(&g[first..]));
        // the header's station list is not a switch
        assert!(!parts[0].1.contains("work zero"), "{}", parts[0].1);
    }

    #[test]
    fn one_station_posts_as_a_plain_setup() {
        let a = station("Op1", 1, 0.0, &[(24, OpKind::Face), (1, OpKind::Chamfer)]);
        let ctx = cam_paths::Ctx::new(&a.cam, &a.part);
        let stock = Some(ctx.stock);
        let plain = crate::cam_post::gcode_noted(&a.cam, &a.paths, ctx.origin, ctx.safe(), Some(cam_paths::work_zero_note(&a.cam, &a.part)), stock);
        let job = Job::new(vec![a]);
        assert_eq!(job.gcode(), plain);
    }

    #[test]
    fn the_combined_simulation_cuts_both_stations() {
        let a = station("Op1", 1, -50.0, &[(24, OpKind::Face)]);
        let b = station("Op2", 2, 50.0, &[(24, OpKind::Face)]);
        let job = Job::new(vec![a, b]);
        let (c, _, paths, mut sim) = job.sim(300).unwrap();
        assert_eq!(c.ops.len(), 2);
        assert!(c.ops[1].name.starts_with("Op2: "));
        // the move over the top between stations
        assert!(paths[1].moves.len() == 6, "{:?}", paths[1].moves);
        sim.run_to(usize::MAX, usize::MAX);
        // each station's slot: 10 mm at 1 mm deep with a 6 mm tool, in its own stock
        let (s1, s2) = (sim.stock_map(), &job.stations[1]);
        assert!(s1.column(5.0, 0.0).unwrap().1 < 9.5);
        assert!(s1.column(5.0 + s2.offset.x, 0.0).unwrap().1 < 9.5);
        // nothing between the stations
        assert!(s1.column(50.0, 0.0).is_none());
        assert_eq!(job.station_of(paths[1].op), (1, job.stations[1].paths[0].op));
    }
}
