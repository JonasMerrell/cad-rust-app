//! Port of `crackh3d/io/bc_json.py`: boundary conditions from a
//! `selected_faces.json` file (the format `bc_gui.py` writes and the Abaqus /
//! FEniCS consumers read). See the Python module docstring for the schema and
//! the matching rule (a face is selected when its *centre of mass* lies inside
//! a bounding box, within a small tolerance).
//!
//! The structs live here; `fe::solve` re-exports them. `LoadCase` carries the
//! raw magnitude and `is_force` flag: the force-to-traction division needs the
//! selected area, so `fe::solve` does it once the mesh exists.
//!
//! Deviation from Python: `FaceSelection::center` is `Option<[f64; 3]>`; a
//! `center` entry that is not exactly three numbers is dropped (Python keeps
//! any array; the value is only a diagnostic and is never read).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::fe::solve::SolveError;

/// Python raises `ValueError` (message kept) or the OS / JSON decode error.
#[derive(Debug, Error)]
pub enum BcError {
    #[error("{0}")]
    Value(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<BcError> for SolveError {
    fn from(e: BcError) -> Self {
        SolveError::Invalid(e.to_string())
    }
}

type Result<T> = std::result::Result<T, BcError>;

fn value_err<T>(msg: String) -> Result<T> {
    Err(BcError::Value(msg))
}

const COMPONENTS: [&str; 3] = ["x", "y", "z"];
const ROTATIONS: [&str; 3] = ["xrot", "yrot", "zrot"];

/// `FaceSelection`: a bounding box selecting CAD faces by centre of mass.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceSelection {
    pub bbox_lo: [f64; 3],
    pub bbox_hi: [f64; 3],
    pub center: Option<[f64; 3]>,
}

impl FaceSelection {
    /// Python's default tolerance for `contains` / `match_face_tags`.
    pub const DEFAULT_TOL: f64 = 1e-3;

    pub fn contains(&self, p: [f64; 3], tol: f64) -> bool {
        (0..3).all(|k| p[k] >= self.bbox_lo[k] - tol && p[k] <= self.bbox_hi[k] + tol)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplacementCase {
    pub faces: Vec<FaceSelection>,
    /// component index -> prescribed value; absent means free
    pub values: BTreeMap<usize, f64>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadCase {
    pub faces: Vec<FaceSelection>,
    /// unit vector
    pub direction: [f64; 3],
    /// total force if `is_force`, else a traction
    pub magnitude: f64,
    pub is_force: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryConditions {
    pub displacement: Vec<DisplacementCase>,
    pub load: Vec<LoadCase>,
}

impl BoundaryConditions {
    /// Same check as the Python `__post_init__`.
    pub fn new(displacement: Vec<DisplacementCase>, load: Vec<LoadCase>) -> Result<Self> {
        if displacement.is_empty() {
            return value_err(
                "boundary conditions contain no disp_bc case: with only tractions the model has six unconstrained \
                 rigid-body modes and the stiffness matrix is singular"
                    .into(),
            );
        }
        Ok(Self { displacement, load })
    }
}

// ----------------------------------------------------------- Python helpers

/// Python `repr(float)`.
fn pyf(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{x:?}");
    match s.split_once('e') {
        Some((m, e)) => {
            let n: i32 = e.parse().unwrap();
            format!("{m}e{}{:02}", if n < 0 { '-' } else { '+' }, n.abs())
        }
        None => s,
    }
}

/// Python `repr` of a parsed-JSON value (dicts keep file order).
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                pyf(n.as_f64().unwrap())
            }
        }
        Value::String(s) => {
            let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut o = String::new();
            o.push(q);
            for c in s.chars() {
                match c {
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\t' => o.push_str("\\t"),
                    '\r' => o.push_str("\\r"),
                    c if c == q => {
                        o.push('\\');
                        o.push(c);
                    }
                    c => o.push(c),
                }
            }
            o.push(q);
            o
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `float(x)` for the JSON values Python's `float()` accepts.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Python truthiness of a JSON value (for `lv.get(k) or 0.0`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn face_selections(entries: &[Value], whence: &str) -> Result<Vec<FaceSelection>> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let ill = || BcError::Value(format!("{whence}: face entry is missing a well-formed 'bbox' ({})", py_repr(entry)));
        let b = entry.get("bbox").filter(|_| entry.is_object()).ok_or_else(ill)?;
        // np.array([...], dtype=float): a null entry becomes nan, a missing key is a KeyError
        let get = |k: &str| -> Result<f64> {
            match b.get(k) {
                Some(Value::Null) => Ok(f64::NAN),
                Some(v) => py_float(v).ok_or_else(ill),
                None => Err(ill()),
            }
        };
        let lo = [get("xmin")?, get("ymin")?, get("zmin")?];
        let hi = [get("xmax")?, get("ymax")?, get("zmax")?];
        let center = entry.get("center").and_then(|c| c.as_array()).and_then(|a| {
            let v: Vec<f64> = a.iter().filter_map(py_float).collect();
            if v.len() == 3 && a.len() == 3 { Some([v[0], v[1], v[2]]) } else { None }
        });
        out.push(FaceSelection { bbox_lo: lo, bbox_hi: hi, center });
    }
    if out.is_empty() {
        return value_err(format!("{whence}: 'faces' is empty, so this case selects nothing"));
    }
    Ok(out)
}

fn cases<'a>(data: &'a Value, key: &str) -> Result<&'a [Value]> {
    match data.get(key) {
        None => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => value_err(format!("{key} must be a list, got {}", py_repr(other))),
    }
}

/// Port of `parse_bcs`: parse an already-loaded `selected_faces.json` value.
pub fn parse_bcs(data: &Value) -> Result<BoundaryConditions> {
    let mut displacement = Vec::new();
    for (i, case) in cases(data, "disp_bc")?.iter().enumerate() {
        let whence = format!("disp_bc[{i}]");
        let bc = case.get("bc");
        let mut values = BTreeMap::new();
        for (c, key) in COMPONENTS.iter().enumerate() {
            if let Some(v) = bc.and_then(|b| b.get(*key)) {
                if !v.is_null() {
                    match py_float(v) {
                        Some(x) => {
                            values.insert(c, x);
                        }
                        None => return value_err(format!("could not convert string to float: {}", py_repr(v))),
                    }
                }
            }
        }
        if values.is_empty() {
            return value_err(format!(
                "{whence}: every translational component is null, so this case constrains nothing. (Rotational components ({}) are ignored -- continuum solid elements have no rotational degree of freedom.)",
                ROTATIONS.iter().map(|r| format!("'{r}'")).collect::<Vec<_>>().join(", ")
            ));
        }
        let faces = face_selections(face_list(case)?, &whence)?;
        displacement.push(DisplacementCase { faces, values, name: whence });
    }

    let mut load = Vec::new();
    for (i, case) in cases(data, "load_bc")?.iter().enumerate() {
        let whence = format!("load_bc[{i}]");
        let lv = case.get("loading");
        let mut vec = [0.0f64; 3];
        for (c, k) in COMPONENTS.iter().enumerate() {
            if let Some(v) = lv.and_then(|l| l.get(*k)).filter(|v| truthy(v)) {
                vec[c] = py_float(v).ok_or_else(|| BcError::Value(format!("could not convert string to float: {}", py_repr(v))))?;
            }
        }
        let magnitude = ((vec[0] * vec[0] + vec[1] * vec[1]) + vec[2] * vec[2]).sqrt();
        // a zero-magnitude load keeps a well-defined (+x) direction
        let direction = if magnitude > 0.0 { [vec[0] / magnitude, vec[1] / magnitude, vec[2] / magnitude] } else { [1.0, 0.0, 0.0] };
        let is_force = lv.and_then(|l| l.get("type")).and_then(|t| t.as_str()) == Some("force");
        let faces = face_selections(face_list(case)?, &whence)?;
        load.push(LoadCase { faces, direction, magnitude, is_force, name: whence });
    }
    BoundaryConditions::new(displacement, load)
}

fn face_list(case: &Value) -> Result<&[Value]> {
    match case.get("faces") {
        None => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => value_err(format!("'faces' must be a list, got {}", py_repr(other))),
    }
}

/// Port of `load_bcs`: read and parse a `selected_faces.json` file.
pub fn load_bcs(path: impl AsRef<Path>) -> Result<BoundaryConditions> {
    let text = std::fs::read_to_string(path)?;
    parse_bcs(&serde_json::from_str(&text)?)
}

// ------------------------------------------------------------ match_face_tags

/// numpy's `str()` of a 1-D float64 array, for the default print options
/// (precision 8, `floatmode="maxprec"`, no suppression): fixed notation with
/// the decimal points aligned, or scientific when the magnitudes spread.
pub fn np_array_str(v: &[f64]) -> String {
    let finite: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    let nonzero: Vec<f64> = finite.iter().map(|x| x.abs()).filter(|x| *x != 0.0).collect();
    let max_val = finite.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    let min_val = nonzero.iter().fold(f64::INFINITY, |m, x| m.min(*x));
    let exp = !finite.is_empty() && (max_val >= 1e8 || (!nonzero.is_empty() && (min_val < 0.0001 || max_val / min_val > 1000.0)));
    let strs: Vec<String> = if exp {
        // dragon4 "unique" scientific per element, then a common mantissa
        // width (trailing zeros kept) and a common integer-part width
        let uniq: Vec<(String, i32)> = v
            .iter()
            .map(|x| {
                let s = format!("{x:e}");
                let (m, e) = s.split_once('e').unwrap();
                (m.to_string(), e.parse::<i32>().unwrap())
            })
            .collect();
        let frac = |m: &str| m.split_once('.').map_or(0, |p| p.1.len());
        let precision = uniq.iter().map(|(m, _)| frac(m)).max().unwrap_or(0).min(8);
        let strs: Vec<String> = v
            .iter()
            .map(|x| {
                let s = format!("{:.*e}", precision, x);
                let (m, e) = s.split_once('e').unwrap();
                let en: i32 = e.parse().unwrap();
                format!("{m}e{}{:02}", if en < 0 { '-' } else { '+' }, en.abs())
            })
            .collect();
        let pad_left = strs.iter().map(|s| s.split('.').next().unwrap().len()).max().unwrap_or(0);
        strs.iter()
            .map(|s| {
                let (i, rest) = s.split_once('.').unwrap_or((s.as_str(), ""));
                if rest.is_empty() && !s.contains('.') { format!("{i:>pad_left$}") } else { format!("{i:>pad_left$}.{rest}") }
            })
            .collect()
    } else {
        let parts: Vec<(String, String)> = v
            .iter()
            .map(|x| {
                let s = if x.is_nan() {
                    "nan.".to_string()
                } else if x.is_infinite() {
                    if *x > 0.0 { "inf.".into() } else { "-inf.".into() }
                } else {
                    // shortest round-trip digits, at most 8 after the point
                    let mut s = format!("{x:?}");
                    if s.split_once('.').map_or(0, |p| p.1.len()) > 8 || s.contains('e') {
                        s = format!("{:.8}", x);
                        while s.ends_with('0') {
                            s.pop();
                        }
                    }
                    if s.ends_with(".0") {
                        s.pop();
                    }
                    if !s.contains('.') {
                        s.push('.');
                    }
                    s
                };
                let (i, f) = s.split_once('.').unwrap();
                (i.to_string(), f.to_string())
            })
            .collect();
        let pad_left = parts.iter().map(|p| p.0.len()).max().unwrap_or(0);
        let pad_right = parts.iter().map(|p| p.1.len()).max().unwrap_or(0);
        parts.iter().map(|(i, f)| format!("{i:>pad_left$}.{f:<pad_right$}")).collect()
    };
    format!("[{}]", strs.join(" "))
}

fn round4(x: f64) -> f64 {
    // np.round(x, 4) = rint(x * 1e4) / 1e4
    let y = x * 1e4;
    let r = y.round_ties_even();
    r / 1e4
}

/// Port of `match_face_tags`: CAD face tags whose centre of mass falls inside
/// any of `selections`, in model order. Errors when nothing matched.
pub fn match_face_tags(face_centers: &[(i64, [f64; 3])], selections: &[FaceSelection], tol: f64) -> Result<Vec<i64>> {
    if face_centers.is_empty() {
        return value_err("match_face_tags: the model has no surfaces to match against".into());
    }
    let tags: Vec<i64> =
        face_centers.iter().filter(|(_, c)| selections.iter().any(|s| s.contains(*c, tol))).map(|(t, _)| *t).collect();
    if tags.is_empty() {
        let boxes = selections
            .iter()
            .map(|s| format!("[{} .. {}]", np_array_str(&s.bbox_lo.map(round4)), np_array_str(&s.bbox_hi.map(round4))))
            .collect::<Vec<_>>()
            .join("; ");
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for (_, c) in face_centers {
            for k in 0..3 {
                lo[k] = lo[k].min(c[k]);
                hi[k] = hi[k].max(c[k]);
            }
        }
        return value_err(format!(
            "no CAD face centroid falls inside the selection box(es) {boxes}. The model's face centroids span {} .. {} -- check that the JSON and the STEP file are for the same part and in the same units.",
            np_array_str(&lo.map(round4)),
            np_array_str(&hi.map(round4)),
        ));
    }
    Ok(tags)
}
