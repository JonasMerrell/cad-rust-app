//! Photogrammetry: a folder of photos of a part becomes a mesh reference body in a Part
//! Studio, to measure, trace and model against.
//!
//! rCAD runs open-source reconstruction programs as separate processes (it doesn't link
//! them): COLMAP (BSD) finds the cameras and, with an NVIDIA GPU, the dense surface; without
//! one, OpenMVS (AGPL, run as its own programs) makes the dense surface on the CPU. GLOMAP
//! (BSD) can stand in for COLMAP's mapper. The work files go in an `rcad-scan` folder inside
//! the photo folder, so a run that stops can carry on where it was. `photo_mesh.rs` then
//! cleans the mesh: cuts the table away, stands the part on XY, sets the scale (a reference
//! block beside the part, or two marks clicked on a ruler in a photo) and thins it.
//!
//! Photo folders from the capture app (`~/scans/<date>-<name>/`) have a `session.json` with
//! the reference object and which photos show it (CAL_*.jpg); they take part in the
//! reconstruction like the rest.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

use crate::photo_mesh::{self, Cleanup, Mesh};

/// Where the reconstruction programs are, if not on the PATH (saved in the config folder).
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// The `colmap` program (or COLMAP.bat on Windows).
    pub colmap: String,
    /// The folder with OpenMVS's programs (DensifyPointCloud, ReconstructMesh, ...).
    pub openmvs: String,
    /// The `glomap` program (optional, a faster mapper).
    pub glomap: String,
}

fn config_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app").join("photogrammetry.json"))
}

impl Settings {
    pub fn load() -> Settings {
        config_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = config_file() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }
}

/// The programs found.
#[derive(Clone, Default, Debug)]
pub struct Tools {
    pub colmap: Option<PathBuf>,
    /// COLMAP's version line, e.g. "COLMAP 3.9.1 ... with CUDA".
    pub colmap_version: String,
    pub cuda: bool,
    /// Newer COLMAPs renamed SiftExtraction.use_gpu to FeatureExtraction.use_gpu (and
    /// matching, and max_image_size).
    pub new_options: bool,
    pub new_max_size: bool,
    /// OpenMVS's folder.
    pub openmvs: Option<PathBuf>,
    pub glomap: Option<PathBuf>,
    /// COLMAP 4 has GLOMAP built in as `global_mapper`.
    pub global_mapper: bool,
}

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// A program on the PATH (with .exe/.bat on Windows).
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".bat", ".cmd", ""] } else { &[""] };
    std::env::split_paths(&path).flat_map(|d| exts.iter().map(move |x| d.join(format!("{name}{x}")))).find(|p| p.is_file())
}

/// Folders next to rCAD where the programs may have been unpacked.
fn beside_rcad() -> Vec<PathBuf> {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return vec![] };
    vec![dir.join("photogrammetry"), dir.clone()]
}

pub fn find_tools(s: &Settings) -> Tools {
    let mut t = Tools::default();
    let given = |p: &str| (!p.trim().is_empty()).then(|| PathBuf::from(p.trim())).filter(|p| p.exists());
    t.colmap = given(&s.colmap).or_else(|| std::env::var_os("RCAD_COLMAP").map(PathBuf::from)).or_else(|| on_path("colmap")).or_else(|| {
        beside_rcad()
            .into_iter()
            .flat_map(|d| [d.join("colmap").join("COLMAP.bat"), d.join("colmap").join("bin").join(format!("colmap{EXE}")), d.join("colmap").join("colmap")])
            .find(|p| p.is_file())
    });
    if let Some(c) = &t.colmap {
        // "colmap -h" names the build: "... with CUDA" or "... without CUDA"
        if let Ok(o) = quiet(c).arg("-h").output() {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            t.colmap_version = text.lines().take(2).collect::<Vec<_>>().join(" ").trim().to_string();
            t.cuda = text.contains("with CUDA") && !text.contains("without CUDA");
        }
        if let Ok(o) = quiet(c).args(["feature_extractor", "-h"]).output() {
            // COLMAP 4 prints its help on stderr
            let h = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            t.new_options = h.contains("FeatureExtraction.use_gpu");
            t.new_max_size = h.contains("FeatureExtraction.max_image_size");
        }
        t.global_mapper = quiet(c).args(["global_mapper", "-h"]).output().is_ok_and(|o| o.status.success());
    }
    let mvs_dir = |d: PathBuf| d.join(format!("DensifyPointCloud{EXE}")).is_file().then_some(d);
    t.openmvs = given(&s.openmvs)
        .and_then(mvs_dir)
        .or_else(|| std::env::var_os("RCAD_OPENMVS").map(PathBuf::from).and_then(mvs_dir))
        .or_else(|| on_path("DensifyPointCloud").and_then(|p| p.parent().map(Path::to_path_buf)))
        .or_else(|| ["/usr/local/bin/OpenMVS", "/usr/bin/OpenMVS", "C:\\Program Files\\OpenMVS"].iter().map(PathBuf::from).find_map(mvs_dir))
        .or_else(|| beside_rcad().into_iter().find_map(|d| mvs_dir(d.join("openmvs"))));
    t.glomap = given(&s.glomap).or_else(|| on_path("glomap"));
    t
}

/// A command with no console window on Windows and Qt told it has no screen.
fn quiet(p: &Path) -> Command {
    let mut c = Command::new(p);
    c.env("QT_QPA_PLATFORM", "offscreen").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// How the dense surface is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// COLMAP on the GPU if it has CUDA, otherwise OpenMVS, otherwise the rough sparse mesh.
    #[default]
    Auto,
    /// COLMAP's dense stereo (needs an NVIDIA GPU) and Poisson meshing.
    Colmap,
    /// OpenMVS dense point cloud and mesh (CPU).
    Openmvs,
    /// COLMAP's sparse points meshed: quick and rough, no extra programs.
    Sparse,
}

impl Method {
    pub const ALL: [Method; 4] = [Method::Auto, Method::Colmap, Method::Openmvs, Method::Sparse];

    pub fn label(self) -> &'static str {
        match self {
            Method::Auto => "Automatic",
            Method::Colmap => "COLMAP dense (NVIDIA GPU)",
            Method::Openmvs => "OpenMVS dense (CPU)",
            Method::Sparse => "Sparse points only (rough)",
        }
    }

    /// The method `Auto` stands for with these tools, or why none can run.
    pub fn resolve(self, t: &Tools) -> Result<Method, String> {
        if t.colmap.is_none() {
            return Err(
                "COLMAP isn't installed: get it from colmap.github.io (Windows: the CUDA build from its GitHub releases; Linux: `apt install colmap` or build it), or set its path"
                    .into(),
            );
        }
        Ok(match self {
            Method::Auto if t.cuda => Method::Colmap,
            Method::Auto if t.openmvs.is_some() => Method::Openmvs,
            Method::Auto => Method::Sparse,
            Method::Colmap if !t.cuda => return Err("this COLMAP was built without CUDA, so it can't make the dense surface: use OpenMVS, or a COLMAP CUDA build".into()),
            Method::Openmvs if t.openmvs.is_none() => return Err("OpenMVS isn't installed: get it from github.com/cdcseacave/openMVS/releases, or set its folder".into()),
            m => m,
        })
    }
}

/// Detail of the reconstruction (and how long it takes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Fast,
    #[default]
    Normal,
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Fast, Quality::Normal, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Fast => "Fast",
            Quality::Normal => "Normal",
            Quality::High => "High",
        }
    }

    /// Longest image side for feature finding and for the dense step.
    fn sizes(self) -> (u32, u32) {
        match self {
            Quality::Fast => (1600, 1200),
            Quality::Normal => (2400, 2000),
            Quality::High => (3200, 3200),
        }
    }
}

/// The capture app's session.json (only what rCAD uses).
#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Session {
    pub name: String,
    pub notes: String,
    pub complete: Option<bool>,
    pub ruler_mm: Option<f64>,
    pub reference: Option<Reference>,
    pub calibration_photos: Vec<String>,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Reference {
    /// "123-block", "246-block", "ruler", "box" or "sphere".
    pub kind: String,
    pub label: String,
    pub dims_mm: Vec<f64>,
}

impl Reference {
    /// A block's sizes (a ball as d × d × d) for finding it in the mesh; None for a ruler.
    pub fn block(&self) -> Option<[f64; 3]> {
        match (self.kind.as_str(), self.dims_mm.as_slice()) {
            ("ruler", _) => None,
            ("sphere", [d, ..]) => Some([*d; 3]),
            (_, [a, b, c, ..]) => Some([*a, *b, *c]),
            ("123-block", _) => Some([25.4, 50.8, 76.2]),
            ("246-block", _) => Some([50.8, 101.6, 152.4]),
            _ => None,
        }
    }
}

pub fn read_session(photos: &Path) -> Option<Session> {
    serde_json::from_str(&std::fs::read_to_string(photos.join("session.json")).ok()?).ok()
}

/// The photos in a folder (JPEG, PNG, TIFF), sorted by name.
pub fn photos_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            [".jpg", ".jpeg", ".png", ".tif", ".tiff"].iter().any(|x| l.ends_with(x))
        })
        .collect();
    v.sort();
    v
}

/// What to reconstruct, and how.
#[derive(Clone, Debug)]
pub struct Job {
    pub photos: PathBuf,
    pub method: Method,
    pub quality: Quality,
    /// Use the GPU for feature finding and matching (COLMAP with CUDA).
    pub gpu: bool,
    /// Solve all the cameras at once (COLMAP 4's global mapper, or GLOMAP) when available.
    pub glomap: bool,
}

impl Job {
    pub fn work(&self) -> PathBuf {
        self.photos.join("rcad-scan")
    }
}

/// Progress of a run, sent as it goes.
#[derive(Clone, Debug)]
pub enum Progress {
    /// Step index, number of steps, step name.
    Step(usize, usize, String),
    /// Fraction done of the current step, when the program says.
    Fraction(f32),
    Line(String),
}

struct Step {
    name: &'static str,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
}

/// What the run left behind, for the finishing step.
#[derive(Clone, Debug)]
pub struct Reconstruction {
    pub work: PathBuf,
    /// The mesh the dense step made.
    pub mesh: PathBuf,
    /// The undistorted photos and their camera model (for clicking ruler marks).
    pub images: PathBuf,
    pub model: PathBuf,
    pub method: Method,
}

/// The run's settings, kept so a re-run with the same photos and settings carries on.
#[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
struct State {
    photos: Vec<String>,
    method: String,
    quality: String,
    done: Vec<String>,
}

fn plan(job: &Job, t: &Tools, method: Method) -> Result<Vec<Step>, String> {
    let colmap = t.colmap.clone().ok_or("COLMAP isn't installed")?;
    let w = job.work();
    let p = |x: &Path| x.display().to_string();
    let (feat_size, dense_size) = job.quality.sizes();
    let gpu = if job.gpu && t.cuda { "1" } else { "0" };
    let (fe, fm) = if t.new_options { ("FeatureExtraction", "FeatureMatching") } else { ("SiftExtraction", "SiftMatching") };
    let n = photos_in(&job.photos).len();
    let mut steps = vec![Step {
        name: "Finding features",
        program: colmap.clone(),
        args: vec![
            "feature_extractor".into(),
            "--database_path".into(),
            p(&w.join("database.db")),
            "--image_path".into(),
            p(&job.photos),
            "--image_list_path".into(),
            p(&w.join("photos.txt")),
            "--ImageReader.single_camera".into(),
            "1".into(),
            "--ImageReader.camera_model".into(),
            "OPENCV".into(),
            format!("--{fe}.use_gpu"),
            gpu.into(),
            if t.new_max_size { "--FeatureExtraction.max_image_size" } else { "--SiftExtraction.max_image_size" }.into(),
            feat_size.to_string(),
        ],
        cwd: w.clone(),
    }];
    // many photos taken walking round the part: match neighbours (and loop closures) only
    let matcher = if n > 150 { "sequential_matcher" } else { "exhaustive_matcher" };
    steps.push(Step {
        name: "Matching photos",
        program: colmap.clone(),
        args: vec![matcher.into(), "--database_path".into(), p(&w.join("database.db")), format!("--{fm}.use_gpu"), gpu.into()],
        cwd: w.clone(),
    });
    let sparse = w.join("sparse");
    let solve = |name: &'static str, program: PathBuf, cmd: &str| Step {
        name,
        program,
        args: vec![cmd.into(), "--database_path".into(), p(&w.join("database.db")), "--image_path".into(), p(&job.photos), "--output_path".into(), p(&sparse)],
        cwd: w.clone(),
    };
    // a global solve places every camera at once: often ten times faster than adding photos
    // one by one (COLMAP 4 has GLOMAP built in; older ones need the separate program)
    steps.push(match (job.glomap, t.global_mapper, &t.glomap) {
        (true, true, _) => solve("Solving cameras (global)", colmap.clone(), "global_mapper"),
        (true, false, Some(g)) => solve("Solving cameras (GLOMAP)", g.clone(), "mapper"),
        _ => solve("Solving cameras", colmap.clone(), "mapper"),
    });
    let dense = w.join("dense");
    // sparse/0 is the main model (the biggest one is picked before this step runs)
    steps.push(Step {
        name: "Undistorting photos",
        program: colmap.clone(),
        args: vec![
            "image_undistorter".into(),
            "--image_path".into(),
            p(&job.photos),
            "--input_path".into(),
            p(&sparse.join("0")),
            "--output_path".into(),
            p(&dense),
            "--output_type".into(),
            "COLMAP".into(),
            "--max_image_size".into(),
            dense_size.to_string(),
        ],
        cwd: w.clone(),
    });
    match method {
        Method::Colmap => {
            // the geometric-consistency pass runs every photo a second time: about twice as
            // long for a cleaner surface, so Fast leaves it out
            let geom = job.quality != Quality::Fast;
            steps.push(Step {
                name: "Dense stereo (GPU)",
                program: colmap.clone(),
                args: vec!["patch_match_stereo".into(), "--workspace_path".into(), p(&dense), "--PatchMatchStereo.geom_consistency".into(), geom.to_string()],
                cwd: w.clone(),
            });
            steps.push(Step {
                name: "Fusing depth maps",
                program: colmap.clone(),
                args: vec![
                    "stereo_fusion".into(),
                    "--workspace_path".into(),
                    p(&dense),
                    "--input_type".into(),
                    if geom { "geometric" } else { "photometric" }.into(),
                    "--output_path".into(),
                    p(&dense.join("fused.ply")),
                ],
                cwd: w.clone(),
            });
            steps.push(Step {
                name: "Meshing",
                program: colmap.clone(),
                args: vec!["poisson_mesher".into(), "--input_path".into(), p(&dense.join("fused.ply")), "--output_path".into(), p(&dense.join("mesh.ply"))],
                cwd: w.clone(),
            });
        }
        Method::Openmvs => {
            let mvs = t.openmvs.clone().ok_or("OpenMVS isn't installed")?;
            let level = match job.quality {
                Quality::Fast => "2",
                Quality::Normal => "1",
                Quality::High => "0",
            };
            steps.push(Step {
                name: "Preparing OpenMVS",
                program: mvs.join(format!("InterfaceCOLMAP{EXE}")),
                args: vec!["-i".into(), ".".into(), "-o".into(), "scene.mvs".into(), "--image-folder".into(), "images".into()],
                cwd: dense.clone(),
            });
            steps.push(Step {
                name: "Dense point cloud (CPU)",
                program: mvs.join(format!("DensifyPointCloud{EXE}")),
                args: vec!["scene.mvs".into(), "--resolution-level".into(), level.into(), "-w".into(), ".".into()],
                cwd: dense.clone(),
            });
            steps.push(Step {
                name: "Meshing",
                program: mvs.join(format!("ReconstructMesh{EXE}")),
                args: vec!["scene_dense.mvs".into(), "-p".into(), "scene_dense.ply".into(), "-o".into(), "mesh.ply".into(), "-w".into(), ".".into()],
                cwd: dense.clone(),
            });
        }
        _ => {
            steps.push(Step {
                name: "Meshing sparse points",
                program: colmap.clone(),
                args: vec!["delaunay_mesher".into(), "--input_path".into(), p(&dense), "--input_type".into(), "sparse".into(), "--output_path".into(), p(&dense.join("mesh.ply"))],
                cwd: w.clone(),
            });
        }
    }
    Ok(steps)
}

/// Run the reconstruction (in a background thread: it takes minutes to hours). Steps a
/// previous run with the same photos and settings finished are skipped.
pub fn run(job: &Job, tools: &Tools, progress: &Sender<Progress>, cancel: &Arc<AtomicBool>) -> Result<Reconstruction, String> {
    let method = job.method.resolve(tools)?;
    let photos = photos_in(&job.photos);
    if photos.len() < 3 {
        return Err(format!("{} has {} photos: take at least 20, all the way round the part", job.photos.display(), photos.len()));
    }
    if let Some(s) = read_session(&job.photos)
        && s.complete == Some(false)
    {
        return Err("this capture session is still uploading from the phone: wait until it's finished".into());
    }
    let w = job.work();
    std::fs::create_dir_all(&w).map_err(|e| format!("can't make {}: {e}", w.display()))?;
    let state_path = w.join("state.json");
    let mut state: State = std::fs::read_to_string(&state_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let (m, q) = (format!("{method:?}"), format!("{:?}", job.quality));
    if state.photos != photos {
        // new or changed photos: start again (the database would mix old and new)
        state = State::default();
        let _ = std::fs::remove_file(w.join("database.db"));
        let _ = std::fs::remove_dir_all(w.join("sparse"));
        let _ = std::fs::remove_dir_all(w.join("dense"));
    } else if state.method != m || state.quality != q {
        // the cameras still hold; redo the dense part
        state.done.retain(|d| ["Finding features", "Matching photos", "Solving cameras", "Solving cameras (GLOMAP)", "Solving cameras (global)"].contains(&d.as_str()));
        let _ = std::fs::remove_dir_all(w.join("dense"));
    }
    state.photos = photos.clone();
    state.method = m;
    state.quality = q;
    std::fs::write(w.join("photos.txt"), photos.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::create_dir_all(w.join("sparse")).map_err(|e| e.to_string())?;
    let steps = plan(job, tools, method)?;
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(w.join("log.txt")).ok();
    for (i, s) in steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let _ = progress.send(Progress::Step(i, steps.len(), s.name.into()));
        if state.done.iter().any(|d| d == s.name) {
            continue;
        }
        if s.name == "Undistorting photos" {
            pick_main_model(&w.join("sparse"), photos.len())?;
        }
        std::fs::create_dir_all(&s.cwd).map_err(|e| e.to_string())?;
        run_step(s, progress, cancel, &mut log, photos.len())?;
        state.done.push(s.name.into());
        let _ = std::fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap_or_default());
    }
    let dense = w.join("dense");
    let mesh = dense.join("mesh.ply");
    if !mesh.is_file() {
        return Err("the reconstruction finished without a mesh: see rcad-scan/log.txt".into());
    }
    Ok(Reconstruction { work: w.clone(), mesh, images: dense.join("images"), model: dense.join("sparse"), method })
}

/// COLMAP may split a scene it can't join into several models (sparse/0, sparse/1, ...):
/// make the one with the most photos sparse/0, and say if most photos didn't fit.
fn pick_main_model(sparse: &Path, photos: usize) -> Result<(), String> {
    let mut models: Vec<(PathBuf, usize)> = std::fs::read_dir(sparse)
        .map_err(|_| "the camera solve made no model: the photos may not overlap enough".to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("images.bin").is_file())
        .map(|p| {
            let n = photo_mesh::read_cameras(&p).map(|c| c.len()).unwrap_or(0);
            (p, n)
        })
        .collect();
    models.sort_by_key(|m| std::cmp::Reverse(m.1));
    let Some((best, n)) = models.first().cloned() else {
        return Err("the camera solve placed no photos: they need to overlap more (move a little between shots) and the part needs texture".into());
    };
    if n * 2 < photos {
        log::warn!("photogrammetry: only {n} of {photos} photos were placed");
    }
    let zero = sparse.join("0");
    if best != zero {
        let tmp = sparse.join("main");
        std::fs::rename(&best, &tmp).map_err(|e| e.to_string())?;
        if zero.exists() {
            std::fs::rename(&zero, &best).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &zero).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_step(s: &Step, progress: &Sender<Progress>, cancel: &Arc<AtomicBool>, log: &mut Option<std::fs::File>, photos: usize) -> Result<(), String> {
    use std::io::Write;
    if let Some(l) = log {
        let _ = writeln!(l, "\n== {}: {} {}", s.name, s.program.display(), s.args.join(" "));
    }
    let mut child = quiet(&s.program)
        .args(&s.args)
        .current_dir(&s.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start {}: {e}", s.program.display()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    for out in
        [child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>)].into_iter().flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            // OpenMVS rewrites its progress line with \r
            let mut r = BufReader::new(out);
            let mut buf = vec![];
            while let Ok(n) = r.read_until(b'\n', &mut buf) {
                if n == 0 {
                    break;
                }
                for part in String::from_utf8_lossy(&buf).split('\r') {
                    let l = part.trim();
                    if !l.is_empty() {
                        let _ = tx.send(l.to_string());
                    }
                }
                buf.clear();
            }
        });
    }
    drop(tx);
    let mut tail: Vec<String> = vec![];
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(l) => {
                if let Some(f) = fraction(&l, photos) {
                    let _ = progress.send(Progress::Fraction(f));
                }
                if let Some(lg) = log {
                    let _ = writeln!(lg, "{l}");
                }
                let _ = progress.send(Progress::Line(l.clone()));
                tail.push(l);
                if tail.len() > 12 {
                    tail.remove(0);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("stopped".into());
        }
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    if !st.success() {
        let why = tail
            .iter()
            .rev()
            .find(|l| l.contains("rror") || l.contains("ERROR") || l.contains("failed") || l.contains("Check failed"))
            .cloned()
            .or_else(|| tail.last().cloned())
            .unwrap_or_default();
        return Err(format!("{} failed ({st}): {why}", s.name));
    }
    Ok(())
}

/// A fraction done from a program's progress line: COLMAP's "[12/40]" and "Registering
/// image #5 (12)", OpenMVS's "(45.67%".
fn fraction(l: &str, photos: usize) -> Option<f32> {
    if let Some(i) = l.find('%') {
        let head = &l[..i];
        let num: String = head.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
        if let Ok(v) = num.parse::<f32>() {
            return Some((v / 100.0).clamp(0.0, 1.0));
        }
    }
    if let Some(a) = l.find('[') {
        let inner = &l[a + 1..l[a..].find(']').map(|b| a + b)?];
        let first = inner.split(',').next()?;
        let (x, y) = first.split_once('/')?;
        let (x, y) = (x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?);
        return (y > 0.0).then(|| (x / y).clamp(0.0, 1.0));
    }
    if l.starts_with("Registering image") {
        let n: f32 = l.rsplit('(').next()?.trim_end_matches(')').trim().parse().ok()?;
        return Some((n / photos.max(1) as f32).clamp(0.0, 1.0));
    }
    None
}

// ------------------------------------------------------------------ finishing

/// How to scale the mesh.
#[derive(Clone, Debug, Default)]
pub enum Scale {
    /// Leave it in the reconstruction's units (fix it later with the Import's scale).
    #[default]
    None,
    /// Millimetres per unit.
    Factor(f64),
    /// Two marks clicked on a photo (undistorted, pixels) that are `mm` apart.
    Clicks { photo: String, a: [f64; 2], b: [f64; 2], mm: f64 },
    /// Find the session's reference block (or these sizes) beside the part.
    Reference(Option<[f64; 3]>),
}

/// A finished scan, ready to become an Import feature.
pub struct Scan {
    /// Levelled, cleaned and thinned, in reconstruction units.
    pub mesh: Mesh,
    /// Millimetres per unit (the Import's scale), 1 when unknown.
    pub scale: f64,
    pub scaled: bool,
    pub notes: Vec<String>,
}

/// Clean the reconstructed mesh and work out its scale.
pub fn finish(r: &Reconstruction, photos: &Path, cleanup: &Cleanup, scale: &Scale) -> Result<Scan, String> {
    let mut raw = photo_mesh::read_ply(&std::fs::read(&r.mesh).map_err(|e| format!("can't read {}: {e}", r.mesh.display()))?)?;
    let mut trim_note = None;
    let fused = r.work.join("dense").join("fused.ply");
    if r.method == Method::Colmap && cleanup.remove_table {
        // Poisson closes the surface over what the photos never saw: trim it back to the points
        if let Ok(pts) = std::fs::read(&fused).map_err(|e| e.to_string()).and_then(|b| photo_mesh::read_points(&b)) {
            let before = raw.t.len();
            let trimmed = photo_mesh::trim_to_points(&raw, &pts, 2.0 * raw.median_edge());
            if trimmed.t.len() > 100 {
                trim_note = Some(format!("trimmed the surface to the {:.0}% that has photo points under it", 100.0 * trimmed.t.len() as f64 / before.max(1) as f64));
                raw = trimmed;
            }
        }
    }
    let cams = photo_mesh::read_cameras(&r.model).unwrap_or_default();
    // where the photos were taken from and which way they look (towards the part)
    let rig: Vec<(glam::DVec3, glam::DVec3)> = cams.iter().map(|c| (c.centre(), c.r.transpose() * glam::DVec3::Z)).collect();
    let mut c = cleanup.clone();
    if let Scale::Reference(dims) = scale {
        c.reference = dims.or_else(|| read_session(photos).and_then(|s| s.reference).and_then(|r| r.block()));
        if c.reference.is_none() {
            return Err("no reference block to look for: give its sizes, or click two ruler marks".into());
        }
    }
    let mut cleaned = photo_mesh::clean(&raw, &c, &rig);
    // a block "found" that makes the part bigger than a room is a false match
    if let Some(s) = cleaned.scale {
        let (lo, hi) = cleaned.mesh.bbox();
        let size = (hi - lo).max_element() * s;
        if size > 3000.0 {
            cleaned.notes.push(format!("ignored the reference match: it would make the part {:.1} m across. Set the scale another way", size / 1000.0));
            cleaned.scale = None;
        }
    }
    let mut notes: Vec<String> = trim_note.into_iter().chain(cleaned.notes.clone()).collect();
    let factor = match scale {
        Scale::None => None,
        Scale::Factor(f) => Some(*f),
        Scale::Reference(_) => cleaned.scale,
        Scale::Clicks { photo, a, b, mm } => {
            let cam = cams.iter().find(|c| &c.name == photo).ok_or_else(|| format!("{photo} isn't one of the placed photos"))?;
            let s = photo_mesh::scale_from_clicks(&raw, cleaned.table, cam, *a, *b, *mm)?;
            notes.push(format!("scale from the marks clicked on {photo}: {s:.4} mm per unit"));
            Some(s)
        }
    };
    // keep the full-detail cleaned mesh next to the photos
    let _ = std::fs::write(r.work.join("part.ply"), photo_mesh::write_ply(&cleaned.mesh));
    Ok(Scan { mesh: cleaned.mesh, scale: factor.unwrap_or(1.0), scaled: factor.is_some(), notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_fractions() {
        assert_eq!(fraction("Processed file [12/40]", 40), Some(0.3));
        assert_eq!(fraction("Matching block [1/4, 1/4]", 40), Some(0.25));
        assert_eq!(fraction("Registering image #3 (20)", 40), Some(0.5));
        assert!((fraction("Estimated depth-maps 12 (31.58%, 1m2s, ETA 2m)...", 40).unwrap() - 0.3158).abs() < 1e-4);
        assert_eq!(fraction("Elapsed time: 0.843 [minutes]", 40), None);
    }

    #[test]
    fn session_reference() {
        let s: Session = serde_json::from_str(r#"{"name":"bracket","complete":true,"reference":{"kind":"123-block","label":"1-2-3 block","dims_mm":[25.4,50.8,76.2]},"calibration_photos":["CAL_0007.jpg"],"camera":{"iso":100}}"#).unwrap();
        assert_eq!(s.reference.unwrap().block(), Some([25.4, 50.8, 76.2]));
        let r = Reference { kind: "sphere".into(), label: String::new(), dims_mm: vec![25.0] };
        assert_eq!(r.block(), Some([25.0; 3]));
        let r = Reference { kind: "ruler".into(), label: String::new(), dims_mm: vec![100.0] };
        assert_eq!(r.block(), None);
    }

    #[test]
    fn methods_resolve() {
        let none = Tools::default();
        assert!(Method::Auto.resolve(&none).is_err());
        let cpu = Tools { colmap: Some("colmap".into()), ..Default::default() };
        assert_eq!(Method::Auto.resolve(&cpu).unwrap(), Method::Sparse);
        assert!(Method::Colmap.resolve(&cpu).is_err());
        let mvs = Tools { openmvs: Some("mvs".into()), ..cpu.clone() };
        assert_eq!(Method::Auto.resolve(&mvs).unwrap(), Method::Openmvs);
        let gpu = Tools { cuda: true, ..mvs };
        assert_eq!(Method::Auto.resolve(&gpu).unwrap(), Method::Colmap);
    }
}
