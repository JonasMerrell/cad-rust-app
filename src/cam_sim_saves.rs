//! A CAM simulation's state at the start of each toolpath, kept on disk, so a run whose first toolpaths are
//! the same as an earlier run's picks up where they end instead of simulating them again: after an edit to
//! one operation, the operations before it are read back, not simulated.
//!
//! A state is kept under a key that chains the app version, a format number, the setup (without its
//! operations and tools), the machine model's file, everything the run starts from (grid, stock, part,
//! fixtures, what it records), and then, toolpath by toolpath, each one's operation, tool and moves bit for
//! bit. The state at the start of toolpath t depends on exactly those, so it is the same state whatever comes
//! after; a change anywhere before t gives another key. The state is everything the run has done and every
//! running total it goes on from (`Sim::state_bytes`), so a run picked up there goes on exactly as one
//! simulated from the start: the same stock, problems, loads and results bit for bit.
//!
//! Only states that took a while to reach are kept ([`WORTH`] of simulating since the last one kept or picked
//! up), which bounds both the disk they take and the time spent writing them. Entries go in a `cam-ops`
//! folder of the build cache (`build_cache::dirs_for`), trimmed to [`LIMIT`], least recently used first.
//! `RCAD_NO_BUILD_CACHE=1` turns this off too.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::cam::Cam;
use crate::cam_paths::{Move, Toolpath};
use crate::cam_sim::Sim;
use crate::cam_sim_cache::Fnv;

/// Bump when the simulator's state or results change in a way the app version doesn't capture.
const FORMAT: u32 = 2;
/// The folder inside a build-cache folder.
pub const SUB: &str = "cam-ops";
const EXT: &str = "state.gz";
const MAGIC: &[u8; 8] = b"RCADOPS1";
/// Moves worked out from a run (`keep_moves`).
const MAGIC_MOVES: &[u8; 8] = b"RCADMVS1";
/// Most one folder may hold before the least recently used entries go.
const LIMIT: u64 = 1 << 30;
/// Seconds of simulating a state has to save before it is kept.
pub const WORTH: f64 = 2.0;

/// The build-cache folders of the document being worked on (`set_doc`).
static DIRS: Mutex<Vec<PathBuf>> = Mutex::new(vec![]);

/// The document whose cache folders the saves go to (None: not saved yet, the user's cache folder).
pub fn set_doc(doc: Option<&Path>) {
    *DIRS.lock().unwrap_or_else(|e| e.into_inner()) = crate::build_cache::dirs_for(doc);
}

/// The build-cache folders `set_doc` chose (empty: none, or turned off).
pub fn build_dirs() -> Vec<PathBuf> {
    DIRS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Where one run keeps and finds its states.
#[derive(Clone)]
pub struct Saves {
    dirs: Vec<PathBuf>,
    /// (move where toolpath t starts, key of the state there) for t = 1..=n, one per move (the last toolpath
    /// starting there), in order.
    at: Vec<(usize, u64)>,
    /// Entries known not to be on disk.
    missing: Vec<bool>,
    /// The first entry after the move the run is at.
    next: usize,
    /// When the run last started simulating, kept a state or picked one up.
    since: Option<std::time::Instant>,
    /// Seconds of simulating a state has to save before it is kept ([`WORTH`]).
    pub(crate) worth: f64,
    /// The move the run was last picked up at.
    pub(crate) picked: Option<usize>,
    /// Per toolpath, the key of the run up to its end (what anything worked out from the run up to there
    /// depends on: `links_key`).
    pub(crate) path_keys: Vec<u64>,
}

impl Saves {
    /// The move the next state is at (usize::MAX: none).
    pub fn next_at(&self) -> usize {
        self.at.get(self.next).map(|a| a.0).unwrap_or(usize::MAX)
    }
}

/// Hash a setup without its operations and tools (each toolpath brings its own), nor what only names it or
/// goes into its G-code.
fn hash_setup(h: &mut Fnv, cam: &Cam) {
    let mut c = cam.clone();
    c.ops.clear();
    c.tools.clear();
    c.next_id = 0;
    c.program = 0;
    c.name.clear();
    c.post.clear();
    c.post_text = Default::default();
    std::hash::Hasher::write(h, serde_json::to_string(&c).unwrap_or_default().as_bytes());
}

fn hash_move(h: &mut Fnv, m: &Move) {
    use std::hash::Hasher;
    let mut f = |x: f64| h.write_u64(x.to_bits());
    match *m {
        Move::Rapid(p) => {
            f(0.0);
            p.into_iter().for_each(&mut f);
        }
        Move::Feed(p, v) => {
            f(1.0);
            p.into_iter().for_each(&mut f);
            f(v);
        }
        Move::Tap { z, r, pitch } => [2.0, z, r, pitch].into_iter().for_each(f),
        Move::Dwell(s) => [3.0, s].into_iter().for_each(f),
    }
}

/// Keep and pick up `sim`'s states at the start of each of `paths` (the toolpaths it was made with, and
/// `cam` the setup it ran them in), in the folders the document uses (`set_doc`). `extra`: anything else the
/// run depends on that the setup and its start don't show (a job's placement of its stations). Call it once
/// the run is set up (start stock, machine, recording) and before it runs.
pub fn keep_ops(sim: &mut Sim, cam: &Cam, paths: &[Toolpath], extra: u64) {
    use std::hash::Hasher;
    let dirs: Vec<PathBuf> = build_dirs().iter().map(|d| d.join(SUB)).collect();
    if dirs.is_empty() || sim.done != 0 {
        return;
    }
    let mut h = Fnv::default();
    h.write(format!("rcad {} cam ops {FORMAT}\n", env!("CARGO_PKG_VERSION")).as_bytes());
    hash_setup(&mut h, cam);
    if sim.machine.is_some() && !cam.machine.model.is_empty() {
        let Some(bytes) = crate::cam_machine::path_of(&cam.machine.model).and_then(|p| std::fs::read(p).ok()) else { return };
        h.write(&bytes);
    }
    if let Some(m) = &sim.machine {
        // where the machine stands (a job places it by its first station)
        for x in m.placed.shift.to_array().into_iter().chain([m.placed.step]) {
            h.write_u64(x.to_bits());
        }
    }
    h.write_u64(extra);
    sim.hash_start(&mut h);
    let mut key = h.finish();
    // the toolpaths one by one: where each starts in the run's moves and the key of the state there
    let mut at: Vec<(usize, u64)> = vec![];
    let mut path_keys = vec![];
    let mut moves = 0usize;
    for tp in paths {
        let mut h = Fnv::default();
        h.write_u64(key);
        h.write_u64(tp.op);
        h.write_u64(tp.tool);
        let tool = cam.path_tool(tp.op, tp.tool);
        h.write(serde_json::to_string(&(cam.op(tp.op), tool.as_deref())).unwrap_or_default().as_bytes());
        h.write_u64(tp.moves.len() as u64);
        tp.moves.iter().for_each(|m| hash_move(&mut h, m));
        key = h.finish();
        path_keys.push(key);
        // (a toolpath without a tool puts no moves in the run: see `Sim::push`)
        if tool.is_some() {
            moves += tp.moves.len();
        }
        match at.last_mut() {
            Some(last) if last.0 == moves => last.1 = key,
            _ if moves > 0 => at.push((moves, key)),
            _ => {}
        }
    }
    if moves != sim.total() {
        log::warn!("CAM simulation states not kept: the toolpaths don't match the run");
        return;
    }
    let n = at.len();
    sim.saves = Some(Box::new(Saves { dirs, at, missing: vec![false; n], next: 0, since: None, worth: WORTH, picked: None, path_keys }));
}

fn file_name(key: u64) -> String {
    format!("{key:016x}.{EXT}")
}

/// Before running `sim` on to `target`: pick it up from the furthest state kept on disk on the way there.
pub fn pick_up(sim: &mut Sim, target: usize) {
    let Some(mut s) = sim.saves.take() else { return };
    for i in (0..s.at.len()).rev() {
        let (m, key) = s.at[i];
        if m <= sim.done || s.missing[i] {
            if m <= sim.done {
                break;
            }
            continue;
        }
        if m > target {
            continue;
        }
        if load(&s.dirs, key, sim) {
            s.since = None;
            s.picked = Some(m);
            break;
        }
        s.missing[i] = true;
    }
    s.next = s.at.partition_point(|a| a.0 <= sim.done);
    s.since.get_or_insert_with(std::time::Instant::now);
    sim.saves = Some(s);
}

/// `sim` has just reached the start of a toolpath (`Saves::next_at`): keep its state there if that took a while.
pub fn save_here(sim: &mut Sim) {
    let Some(s) = sim.saves.as_mut() else { return };
    let i = s.next;
    s.next += 1;
    let Some(&(m, key)) = s.at.get(i) else { return };
    let worth = s.since.is_none_or(|t| t.elapsed().as_secs_f64() >= s.worth);
    if m != sim.done || !worth {
        return;
    }
    let Some(dir) = s.dirs.first().cloned() else { return };
    s.since = Some(std::time::Instant::now());
    if dir.join(file_name(key)).is_file() {
        return;
    }
    let bytes = sim.state_bytes();
    writer().send((dir, key, bytes));
}

/// The key of a toolpath's links worked out by `cam_paths::tighten_links`: the run up to the end of it
/// (`Saves::path_keys`), and where the path before left the tool when it links straight on.
pub fn links_key(path_key: u64, from: Option<glam::DVec3>) -> u64 {
    use std::hash::Hasher;
    let mut h = Fnv::default();
    h.write(b"tighten_links");
    h.write_u64(path_key);
    for x in from.map(|p| p.to_array()).into_iter().flatten() {
        h.write_u64(x.to_bits());
    }
    h.finish()
}

/// Moves kept under `key` (`keep_moves`), from the folders the document uses.
pub fn moves(key: u64) -> Option<Vec<Move>> {
    use crate::cam_sim_cache::Bin;
    let name = file_name(key);
    for d in build_dirs() {
        let p = d.join(SUB).join(&name);
        let Some(b) = p.is_file().then(|| read(&p, key, MAGIC_MOVES)).flatten() else { continue };
        let r = &mut &b[..];
        if let Some(m) = Vec::<Move>::get(r).filter(|_| r.is_empty()) {
            if let Ok(f) = std::fs::File::options().append(true).open(&p) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return Some(m);
        }
    }
    None
}

/// Keep moves worked out from a run under `key` (small: written now).
pub fn keep_moves(key: u64, moves: &[Move]) {
    use crate::cam_sim_cache::Bin;
    let Some(dir) = build_dirs().first().map(|d| d.join(SUB)) else { return };
    let mut b = vec![];
    moves.to_vec().put(&mut b);
    if let Err(e) = write(&dir, key, MAGIC_MOVES, &b) {
        log::warn!("could not keep a toolpath's links in {}: {e}", dir.display());
    }
}

fn load(dirs: &[PathBuf], key: u64, sim: &mut Sim) -> bool {
    let name = file_name(key);
    for p in dirs.iter().map(|d| d.join(&name)).filter(|p| p.is_file()) {
        let Some(bytes) = read(&p, key, MAGIC) else { continue };
        if sim.load_state(&bytes) {
            // the most recently used go last when the folder is trimmed
            if let Ok(f) = std::fs::File::options().append(true).open(&p) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return true;
        }
    }
    false
}

fn read(path: &Path, key: u64, magic: &[u8; 8]) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut b = vec![];
    flate2::read::GzDecoder::new(std::io::BufReader::new(file)).read_to_end(&mut b).ok()?;
    (b.len() >= 16 && &b[..8] == magic && b[8..16] == key.to_le_bytes()).then(|| b.split_off(16))
}

/// The background writer: one thread, a short queue (a run that gets ahead of it waits, rather than holding
/// many states in memory).
struct Writer {
    tx: Mutex<Option<std::sync::mpsc::SyncSender<(PathBuf, u64, Vec<u8>)>>>,
    pending: std::sync::atomic::AtomicUsize,
}

impl Writer {
    fn send(&self, job: (PathBuf, u64, Vec<u8>)) {
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if tx.send(job).is_err() {
                self.pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
}

fn writer() -> &'static Writer {
    static W: std::sync::OnceLock<Writer> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(PathBuf, u64, Vec<u8>)>(2);
        std::thread::spawn(move || {
            for (dir, key, bytes) in rx {
                if let Err(e) = write(&dir, key, MAGIC, &bytes) {
                    log::warn!("could not keep a CAM simulation state in {}: {e}", dir.display());
                }
                writer().pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        Writer { tx: Mutex::new(Some(tx)), pending: Default::default() }
    })
}

/// Wait (up to `limit`) for the states still being written: before the process ends.
pub fn flush(limit: std::time::Duration) {
    let start = std::time::Instant::now();
    while writer().pending.load(std::sync::atomic::Ordering::SeqCst) > 0 && start.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn write(dir: &Path, key: u64, magic: &[u8; 8], bytes: &[u8]) -> std::io::Result<()> {
    let path = dir.join(file_name(key));
    if path.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        // not for version control: it is remade from the document
        let _ = std::fs::write(&ignore, "# rCAD's CAM simulation states, remade from the document when missing\n*\n");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(magic)?;
    gz.write_all(&key.to_le_bytes())?;
    gz.write_all(bytes)?;
    let data = gz.finish()?;
    let tmp = dir.join(format!(".{key:016x}.{}.tmp", std::process::id()));
    let res = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    prune(dir, LIMIT);
    res
}

/// Trim `dir` to `limit` bytes, least recently used first.
fn prune(dir: &Path, limit: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut all: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(&format!(".{EXT}")))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = all.iter().map(|e| e.1).sum();
    if total <= limit {
        return;
    }
    all.sort();
    for (_, size, p) in all {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(p).is_ok() {
            total -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;
    use crate::cam_paths::Part;
    use glam::DVec3;

    /// Three toolpaths with two tools: slots, a plunge, a pass over the slots with the other tool.
    fn setup() -> (Cam, Part, Vec<Toolpath>) {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam { rapid_rate: 6000.0, ..Default::default() };
        cam.machine.max_feed = 1000.0;
        let presets = crate::cam::presets();
        let flat = cam.add_tool(presets.iter().find(|t| t.kind == ToolKind::Flat).unwrap().clone());
        let ball = cam.add_tool(presets.iter().find(|t| t.kind == ToolKind::Ball).unwrap().clone());
        let slot = |y: f64, z: f64| {
            vec![
                Move::Rapid([0.0, y, 15.0]),
                Move::Rapid([0.0, y, 11.0]),
                Move::Feed([0.0, y, z], 300.0),
                Move::Feed([60.0, y, z], 900.0),
                Move::Feed([60.0, y + 6.0, z], 900.0),
                Move::Feed([5.0, y + 6.0, z], 900.0),
                Move::Rapid([5.0, y + 6.0, 15.0]),
            ]
        };
        let pass = |z: f64| {
            let mut m = vec![Move::Rapid([-5.0, 0.0, 15.0]), Move::Feed([-5.0, 0.0, z], 300.0)];
            for k in 0..12 {
                let y = k as f64 * 4.0;
                m.push(Move::Feed([70.0 * ((k % 2) as f64), y, z], 1000.0));
                m.push(Move::Feed([70.0 * ((k % 2) as f64), y + 4.0, z], 1000.0));
            }
            m.push(Move::Rapid([0.0, 48.0, 15.0]));
            m
        };
        let paths = vec![
            Toolpath { op: 1, tool: flat, moves: slot(10.0, 8.0), ..Default::default() },
            Toolpath { op: 2, tool: flat, moves: slot(30.0, 7.0), ..Default::default() },
            Toolpath { op: 3, tool: ball, moves: pass(8.5), ..Default::default() },
        ];
        (cam, part, paths)
    }

    fn sim(cam: &Cam, part: &Part, paths: &[Toolpath], record: bool) -> Sim {
        let stock = (DVec3::new(-10.0, -10.0, 0.0), DVec3::new(90.0, 60.0, 10.0));
        let mut s = Sim::new(cam, part, stock, paths, 150);
        if record {
            s.profile = Some(vec![]);
            s.removed_by_move = Some(vec![(0.0, 0.0); s.total()]);
        }
        s
    }

    #[test]
    fn a_run_picked_up_part_way_ends_the_same() {
        let dir = std::env::temp_dir().join(format!("rcad-cam-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        *DIRS.lock().unwrap() = vec![dir.clone()];
        let (cam, part, paths) = setup();
        let mut edited = paths.clone();
        edited[2].moves = edited[2].moves.iter().map(|m| if let Move::Feed(p, f) = m { Move::Feed([p[0], p[1], p[2] - 0.3], *f) } else { *m }).collect();
        let starts = [paths[0].moves.len(), paths[0].moves.len() + paths[1].moves.len()];
        let total = starts[1] + paths[2].moves.len();
        for record in [false, true] {
            // (paths, where the run should pick up): nothing kept yet; all of it kept; the edit keeps the first two
            for (p, at) in [(&paths, None), (&paths, Some(total)), (&edited, Some(starts[1]))] {
                // straight through, nothing kept
                let mut plain = sim(&cam, &part, p, record);
                plain.run_to(usize::MAX, usize::MAX);
                assert!(plain.removed > 0.0 && !plain.loads.is_empty());
                // kept at every toolpath's start and picked up from there, run in steps as tighten_links does
                let mut kept = sim(&cam, &part, p, record);
                keep_ops(&mut kept, &cam, p, 0);
                kept.saves.as_mut().unwrap().worth = 0.0;
                for t in (0..kept.total()).step_by(5) {
                    kept.run_to(t, usize::MAX);
                }
                kept.run_to(usize::MAX, usize::MAX);
                flush(std::time::Duration::from_secs(10));
                assert_eq!(kept.saves.as_ref().unwrap().picked, at, "record {record}");
                assert!(kept.state_bytes() == plain.state_bytes(), "picked up or not, it ends the same (record {record}, {at:?})");
                assert_eq!(kept.finished_bytes(), plain.finished_bytes());
            }
        }
        // another start (the stock an earlier setup left) is another run
        let mut other = sim(&cam, &part, &paths, false);
        let mut m = other.stock_map();
        m.hi[0] -= 1.0;
        other.start_from(&m);
        keep_ops(&mut other, &cam, &paths, 0);
        other.run_to(usize::MAX, usize::MAX);
        assert_eq!(other.saves.as_ref().unwrap().picked, None);
        // a damaged state is a miss
        let mut damaged = 0;
        for e in std::fs::read_dir(dir.join(SUB)).unwrap().flatten() {
            if e.file_name().to_string_lossy().ends_with(EXT) {
                std::fs::write(e.path(), b"not gzip").unwrap();
                damaged += 1;
            }
        }
        assert!(damaged >= 6, "{damaged}");
        let mut fresh = sim(&cam, &part, &paths, false);
        keep_ops(&mut fresh, &cam, &paths, 0);
        fresh.run_to(usize::MAX, usize::MAX);
        assert_eq!(fresh.saves.as_ref().unwrap().picked, None);
        let mut plain = sim(&cam, &part, &paths, false);
        plain.run_to(usize::MAX, usize::MAX);
        assert!(fresh.state_bytes() == plain.state_bytes());
        *DIRS.lock().unwrap() = vec![];
        let _ = std::fs::remove_dir_all(&dir);
    }
}
