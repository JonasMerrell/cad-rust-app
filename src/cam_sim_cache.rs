//! Finished CAM simulations kept on disk, so a setup simulated once isn't simulated again when the
//! document is opened next time (or by the next `rcad run`).
//!
//! An entry is a finished run's outcome (the stock grid, what cut where, the problems found, the load,
//! air, chip and chatter results), under a key that hashes the app version, the setup, the part, the
//! stock it starts from, the toolpaths, the grid resolution and the machine model's file. Any change
//! to one of them gives another key, so a stale entry is never used.
//!
//! Entries go in a `cam-sim` folder of the build cache (`build_cache::dirs_for`): the project's
//! `.rcad-cache/` or the user's cache folder. Each folder is trimmed to [`LIMIT`], least recently used
//! first. `RCAD_NO_BUILD_CACHE=1` turns this off too.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::DVec3;

use crate::cam::Cam;
use crate::cam_paths::{Part, Toolpath};
use crate::cam_sim::Sim;
use crate::cam_stock::StockMap;

/// Bump when the simulator's results change in a way the app version doesn't capture.
const FORMAT: u32 = 2;
/// The folder inside a build-cache folder.
pub const SUB: &str = "cam-sim";
const EXT: &str = "sim.gz";
const MAGIC: &[u8; 8] = b"RCADSIM1";
/// Most one folder may hold before the least recently used entries go.
const LIMIT: u64 = 1 << 30;

/// Entries still being written in the background.
static WRITING: AtomicUsize = AtomicUsize::new(0);

/// The simulation folders for these build-cache folders (`build_cache::dirs_for`); the first is written to.
pub fn dirs(build_dirs: &[PathBuf]) -> Vec<PathBuf> {
    build_dirs.iter().map(|d| d.join(SUB)).collect()
}

/// The simulation folders for a document at `doc` (None: not saved yet).
pub fn dirs_for(doc: Option<&Path>) -> Vec<PathBuf> {
    dirs(&crate::build_cache::dirs_for(doc))
}

/// FNV-1a, 64 bit: stable across runs, builds and platforms.
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for Fnv {
    fn write(&mut self, buf: &[u8]) {
        for b in buf {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// The key of the run `Sim::new(cam, part, stock, paths, resolution)` (then `start_from(start)`, and the
/// setup's machine) makes, with `extra` for anything else it depends on (a job's stations). None when
/// the machine model can't be read (its file is part of the key).
pub fn key(cam: &Cam, part: &Part, stock: (DVec3, DVec3), paths: &[Toolpath], start: Option<&StockMap>, resolution: usize, extra: u64) -> Option<u64> {
    use std::hash::Hasher;
    let mut h = Fnv::default();
    let f = |h: &mut Fnv, v: f64| h.write(&v.to_bits().to_le_bytes());
    let v3 = |h: &mut Fnv, v: DVec3| v.to_array().into_iter().for_each(|x| f(h, x));
    h.write(format!("rcad {} cam sim {FORMAT}\n", env!("CARGO_PKG_VERSION")).as_bytes());
    h.write(serde_json::to_string(cam).unwrap_or_default().as_bytes());
    h.write(&(part.tris.len() as u64).to_le_bytes());
    for t in &part.tris {
        t.iter().for_each(|p| v3(&mut h, *p));
    }
    h.write(format!("{:?}", part.circles).as_bytes());
    v3(&mut h, stock.0);
    v3(&mut h, stock.1);
    h.write(&crate::cam_verify::toolpath_hash(paths).to_le_bytes());
    for tp in paths {
        f(&mut h, tp.time);
        h.write(&[tp.linked as u8]);
        h.write(serde_json::to_string(&tp.probe).unwrap_or_default().as_bytes());
    }
    match start {
        Some(m) => {
            h.write(b"start");
            [m.x0, m.y0, m.cell].into_iter().for_each(|x| f(&mut h, x));
            h.write(&(m.nx as u64).to_le_bytes());
            h.write(&(m.ny as u64).to_le_bytes());
            for x in m.hi.iter().chain(&m.lo) {
                h.write(&x.to_bits().to_le_bytes());
            }
        }
        None => h.write(b"block"),
    }
    h.write(&(resolution as u64).to_le_bytes());
    h.write(&extra.to_le_bytes());
    if !cam.machine.model.is_empty() {
        let path = crate::cam_machine::path_of(&cam.machine.model)?;
        h.write(&std::fs::read(path).ok()?);
    }
    Some(h.0.max(1))
}

fn file_name(key: u64) -> String {
    format!("{key:016x}.{EXT}")
}

/// Fill `sim` (fresh from `Sim::new` and set up like the run that was stored) with the stored outcome of
/// `key`. False when no folder has it (or it doesn't fit).
pub fn load(dirs: &[PathBuf], key: u64, sim: &mut Sim) -> bool {
    let name = file_name(key);
    for p in dirs.iter().map(|d| d.join(&name)).filter(|p| p.is_file()) {
        let Some(bytes) = read(&p, key) else { continue };
        if sim.load_finished(&bytes) {
            // the most recently used go last when the folder is trimmed
            if let Ok(f) = std::fs::File::options().append(true).open(&p) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return true;
        }
    }
    false
}

fn read(path: &Path, key: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut b = vec![];
    flate2::read::GzDecoder::new(std::io::BufReader::new(file)).read_to_end(&mut b).ok()?;
    (b.len() >= 16 && &b[..8] == MAGIC && b[8..16] == key.to_le_bytes()).then(|| b.split_off(16))
}

/// Keep `sim`'s finished run under `key` in the first of `dirs`, now.
pub fn store_now(dirs: &[PathBuf], key: u64, sim: &Sim) {
    let (Some(dir), Some(bytes)) = (dirs.first(), sim.finished_bytes()) else { return };
    if let Err(e) = write(dir, key, &bytes) {
        log::warn!("could not keep the CAM simulation in {}: {e}", dir.display());
    }
}

/// As [`store_now`], writing on a background thread (the GUI).
pub fn store(dirs: &[PathBuf], key: u64, sim: &Sim) {
    let (Some(dir), Some(bytes)) = (dirs.first().cloned(), sim.finished_bytes()) else { return };
    if dir.join(file_name(key)).is_file() {
        return;
    }
    WRITING.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        if let Err(e) = write(&dir, key, &bytes) {
            log::warn!("could not keep the CAM simulation in {}: {e}", dir.display());
        }
        WRITING.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Wait (up to `limit`) for background writes to finish.
#[allow(dead_code)]
pub fn wait_written(limit: std::time::Duration) {
    let start = std::time::Instant::now();
    while WRITING.load(Ordering::SeqCst) > 0 && start.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn write(dir: &Path, key: u64, bytes: &[u8]) -> std::io::Result<()> {
    let path = dir.join(file_name(key));
    if path.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        // not for version control: it is remade from the document
        let _ = std::fs::write(&ignore, "# rCAD's CAM simulations, remade from the document when missing\n*\n");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(MAGIC)?;
    gz.write_all(&key.to_le_bytes())?;
    gz.write_all(bytes)?;
    let data = gz.finish()?;
    static N: AtomicUsize = AtomicUsize::new(0);
    let tmp = dir.join(format!(".{key:016x}.{}.{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
    let mut all: Vec<(PathBuf, u64, std::time::SystemTime)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(&format!(".{EXT}")))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect();
    let mut total: u64 = all.iter().map(|e| e.1).sum();
    if total <= limit {
        return;
    }
    all.sort_by_key(|e| e.2);
    for (p, size, _) in all {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(p).is_ok() {
            total -= size;
        }
    }
}

/// Run `sim` to the end, or take its outcome from the cache when `key` is there; keep a run made here.
pub fn run(dirs: &[PathBuf], key: Option<u64>, sim: &mut Sim) {
    if let Some(k) = key.filter(|_| !dirs.is_empty()) {
        if load(dirs, k, sim) {
            return;
        }
        sim.run_to(usize::MAX, usize::MAX);
        store_now(dirs, k, sim);
    } else {
        sim.run_to(usize::MAX, usize::MAX);
    }
}

/// A plain little-endian binary encoding for the stored outcome (JSON can't carry the infinities the
/// stock grid and the results use, and skips fields kept for working things out again).
pub trait Bin: Sized {
    fn put(&self, w: &mut Vec<u8>);
    fn get(r: &mut &[u8]) -> Option<Self>;
}

fn take<'a>(r: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if r.len() < n {
        return None;
    }
    let (a, b) = r.split_at(n);
    *r = b;
    Some(a)
}

macro_rules! bin_num {
    ($($t:ty),*) => {$(
        impl Bin for $t {
            fn put(&self, w: &mut Vec<u8>) {
                w.extend_from_slice(&self.to_le_bytes());
            }
            fn get(r: &mut &[u8]) -> Option<Self> {
                Some(<$t>::from_le_bytes(take(r, std::mem::size_of::<$t>())?.try_into().ok()?))
            }
        }
    )*};
}
bin_num!(u8, u32, u64, f32, f64);

impl Bin for usize {
    fn put(&self, w: &mut Vec<u8>) {
        (*self as u64).put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        usize::try_from(u64::get(r)?).ok()
    }
}

impl Bin for bool {
    fn put(&self, w: &mut Vec<u8>) {
        (*self as u8).put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        match u8::get(r)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Bin for String {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        w.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let n = usize::get(r)?;
        String::from_utf8(take(r, n)?.to_vec()).ok()
    }
}

impl<T: Bin> Bin for Vec<T> {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        self.iter().for_each(|x| x.put(w));
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let n = usize::get(r)?;
        // each item takes at least a byte: a damaged length can't ask for more than is left
        if n > r.len() {
            return None;
        }
        (0..n).map(|_| T::get(r)).collect()
    }
}

impl<T: Bin> Bin for Option<T> {
    fn put(&self, w: &mut Vec<u8>) {
        self.is_some().put(w);
        if let Some(x) = self {
            x.put(w);
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(if bool::get(r)? { Some(T::get(r)?) } else { None })
    }
}

impl<T: Bin + Copy + Default, const N: usize> Bin for [T; N] {
    fn put(&self, w: &mut Vec<u8>) {
        self.iter().for_each(|x| x.put(w));
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let mut a = [T::default(); N];
        for x in &mut a {
            *x = T::get(r)?;
        }
        Some(a)
    }
}

impl<A: Bin, B: Bin> Bin for (A, B) {
    fn put(&self, w: &mut Vec<u8>) {
        self.0.put(w);
        self.1.put(w);
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some((A::get(r)?, B::get(r)?))
    }
}

impl Bin for DVec3 {
    fn put(&self, w: &mut Vec<u8>) {
        self.to_array().put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(DVec3::from(<[f64; 3]>::get(r)?))
    }
}

/// `Bin` for a struct, field by field (a field left out of the list doesn't compile).
macro_rules! bin_struct {
    ($t:path { $($f:ident),* $(,)? }) => {
        impl Bin for $t {
            fn put(&self, w: &mut Vec<u8>) {
                $(self.$f.put(w);)*
            }
            fn get(r: &mut &[u8]) -> Option<Self> {
                Some(Self { $($f: Bin::get(r)?),* })
            }
        }
    };
}

impl Bin for crate::cam_sim::EventKind {
    fn put(&self, w: &mut Vec<u8>) {
        use crate::cam_sim::EventKind::*;
        let k: u8 = match self {
            Rapid => 0,
            Shank => 1,
            Holder => 2,
            Gouge => 3,
            Fixture => 4,
            Machine => 5,
            Travel => 6,
            Probe => 7,
            Overload => 8,
            ChamferOverload => 9,
        };
        k.put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        use crate::cam_sim::EventKind::*;
        Some(match u8::get(r)? {
            0 => Rapid,
            1 => Shank,
            2 => Holder,
            3 => Gouge,
            4 => Fixture,
            5 => Machine,
            6 => Travel,
            7 => Probe,
            8 => Overload,
            9 => ChamferOverload,
            _ => return None,
        })
    }
}

bin_struct!(crate::cam_sim::SimEvent { kind, op, step, at, depth, what });
bin_struct!(crate::cam_cutting::Cut { d_eff, vc, fz, angle, h_max, h_mean, mrr, ft_peak, ft_mean, force, power, torque, deflection, stress });
bin_struct!(crate::cam_cutting::Use { power, torque, force, deflection, stress, chip, worst });
bin_struct!(crate::cam_cutting::ToolGeom { diameter, ball, flutes, stickout, flute_length, shank, modulus, stress_limit, neck, neck_length });
bin_struct!(crate::cam_sim::OpLoad { op, step, at, area, width, depth, diameter, max_area, ratio, geometric, feed, rpm, cut, uses });
bin_struct!(crate::cam_sim::OpChatter { op, margin, at, width, depth, stable, rpm, hz, k, kt, geom, zeta, climb, scale });
bin_struct!(crate::cam_sim::OpChips { op, max_fill, at, open_s, slot_s, pocket_s, recut_mm, deep_slot_mm });
impl Bin for crate::cam_paths::Move {
    fn put(&self, w: &mut Vec<u8>) {
        use crate::cam_paths::Move::*;
        match *self {
            Rapid(p) => (0u8, p).put(w),
            Feed(p, f) => (1u8, (p, f)).put(w),
            Tap { z, r, pitch } => (2u8, [z, r, pitch]).put(w),
            Dwell(s) => (3u8, s).put(w),
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        use crate::cam_paths::Move::*;
        Some(match u8::get(r)? {
            0 => Rapid(<[f64; 3]>::get(r)?),
            1 => {
                let (p, f) = <([f64; 3], f64)>::get(r)?;
                Feed(p, f)
            }
            2 => {
                let [z, r, pitch] = <[f64; 3]>::get(r)?;
                Tap { z, r, pitch }
            }
            3 => Dwell(f64::get(r)?),
            _ => return None,
        })
    }
}

bin_struct!(crate::cam_sim::LoadSample { step, width, depth, area, len, feed, power, worst });
bin_struct!(crate::cam_sim::OpAir { op, feed_mm, feed_s, air_mm, air_s, light_mm, light_s, link_mm, link_s });

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;
    use crate::cam_paths::{Move, Part};

    /// A flat end mill slotting across a block, through air and a plunge (it overloads: an event to keep).
    fn setup() -> (Cam, Part, Vec<Toolpath>) {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam { rapid_rate: 6000.0, ..Default::default() };
        cam.machine.max_feed = 1000.0;
        let flat = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap();
        let t = cam.add_tool(flat);
        let moves = vec![
            Move::Rapid([0.0, 20.0, 15.0]),
            Move::Feed([60.0, 20.0, 15.0], 1200.0),
            Move::Rapid([60.0, 20.0, 10.0]),
            Move::Feed([60.0, 20.0, 9.0], 300.0),
            Move::Feed([20.0, 20.0, 9.0], 600.0),
            Move::Rapid([20.0, 20.0, 15.0]),
        ];
        (cam, part, vec![Toolpath { op: 1, tool: t, moves, ..Default::default() }])
    }

    #[test]
    fn a_stored_run_comes_back_the_same() {
        let (cam, part, paths) = setup();
        let stock = (DVec3::new(-10.0, -10.0, 0.0), DVec3::new(90.0, 60.0, 10.0));
        let dir = std::env::temp_dir().join(format!("rcad-cam-sim-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dirs = vec![dir.clone()];
        let k = key(&cam, &part, stock, &paths, None, 120, 0).unwrap();
        assert_eq!(Some(k), key(&cam, &part, stock, &paths, None, 120, 0), "same run, same key");
        assert_ne!(Some(k), key(&cam, &part, stock, &paths, None, 121, 0), "another grid, another key");
        let mut moved = cam.clone();
        moved.rapid_rate = 5000.0;
        assert_ne!(Some(k), key(&moved, &part, stock, &paths, None, 120, 0), "an edited setup, another key");

        let mut ran = Sim::new(&cam, &part, stock, &paths, 120);
        run(&dirs, Some(k), &mut ran);
        assert_eq!(ran.done, ran.total());
        assert!(ran.removed > 0.0 && !ran.events.is_empty() && !ran.loads.is_empty(), "the slot cut, and overloaded");
        assert!(dir.join(file_name(k)).is_file(), "the run was kept");
        assert!(dir.join(".gitignore").is_file());

        let mut back = Sim::new(&cam, &part, stock, &paths, 120);
        assert!(load(&dirs, k, &mut back));
        assert_eq!(back.done, ran.done);
        assert_eq!(back.removed, ran.removed);
        assert_eq!(back.stock, ran.stock);
        assert_eq!(back.lo, ran.lo);
        assert_eq!(back.cuts, ran.cuts);
        assert_eq!(back.gouged, ran.gouged);
        assert_eq!(back.events, ran.events);
        assert_eq!(back.air, ran.air);
        assert_eq!(back.loads, ran.loads);
        assert_eq!(back.tool_pos(), ran.tool_pos());
        assert_eq!(back.mesh(200), ran.mesh(200));

        // a grid of another size doesn't take it
        let mut other = Sim::new(&cam, &part, stock, &paths, 200);
        assert!(!other.load_finished(&ran.finished_bytes().unwrap()));
        // nor does a damaged entry, which is a miss
        std::fs::write(dir.join(file_name(7)), b"not gzip").unwrap();
        let mut fresh = Sim::new(&cam, &part, stock, &paths, 120);
        assert!(!load(&dirs, 7, &mut fresh));
        assert_eq!(fresh.done, 0);
        prune(&dir, 0);
        assert!(!dir.join(file_name(k)).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_round_trips_infinities() {
        let v: Vec<(f32, Option<f64>)> = vec![(f32::NEG_INFINITY, Some(f64::INFINITY)), (1.5, None)];
        let mut w = vec![];
        v.put(&mut w);
        let mut r = w.as_slice();
        assert_eq!(Vec::<(f32, Option<f64>)>::get(&mut r), Some(v));
        assert!(r.is_empty());
        let mut short = &w[..w.len() - 1];
        assert!(Vec::<(f32, Option<f64>)>::get(&mut short).is_none());
    }
}
