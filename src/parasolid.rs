//! Reading Parasolid text files (.x_t, as written by Onshape, SolidWorks, NX, Solid Edge…).
//!
//! The file is turned into STEP here, so the rest of rCAD (and the saved document) only ever
//! sees STEP. Parasolid's B-rep (faces bounded by loops of oriented edges on shared vertices,
//! analytic and B-spline geometry) maps almost one to one onto STEP's advanced B-rep, and
//! OpenCascade's STEP reader then adds the seams and p-curves it needs.
//!
//! An .x_t file is a list of nodes (bodies, faces, edges, surfaces…). Each node type's fields
//! come from a schema. Newer files say, the first time a type appears, how its fields differ
//! from the 13006 base schema; the base layouts below follow the public XT format reference.
//! Only the text format is read, not binary (.x_b).

use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

use glam::{DMat3, DVec3};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Code {
    Int,
    Ptr,
    Real,
    Char,
    Logical,
    Byte,
    Short,
    Wide,
    Vector,
    HVec,
    Interval,
    Box,
}

impl Code {
    fn of(s: &str) -> Option<Code> {
        Some(match s {
            "d" | "t" => Code::Int,
            "p" => Code::Ptr,
            "f" => Code::Real,
            "c" => Code::Char,
            "l" => Code::Logical,
            "u" => Code::Byte,
            "n" => Code::Short,
            "w" => Code::Wide,
            "v" => Code::Vector,
            "h" => Code::HVec,
            "i" => Code::Interval,
            "b" => Code::Box,
            _ => return None,
        })
    }

    fn width(self) -> usize {
        match self {
            Code::Vector | Code::HVec => 3,
            Code::Interval => 2,
            Code::Box => 6,
            _ => 1,
        }
    }
}

/// One field of a node type: `n` is 0 for one value, 1 for a per-node count, else a fixed count.
#[derive(Clone, Debug)]
struct Field {
    name: String,
    code: Code,
    n: u32,
    sent: bool,
}

const GEO: &str = "local_id:d annotations:p owner:p next:p prev:p indirect_owner:p orientation:c ";

/// The 13006 base layouts of the node types rCAD reads, in transmit order.
fn base_spec(t: u16) -> Option<String> {
    let s = match t {
        10 => "highest_node_id:d annotations:p attribute_list:p list4:p surfaces:p curves:p points:p key:p res_size:f res_linear:f ref_instance:p next:p prev:p state:u owner:p type:u sub_instance:p",
        11 => "node_id:d annotations:p type:u part:p transform:p assembly:p next_in_part:p prev_in_part:p next_of_part:p prev_of_part:p",
        12 => "max_local_id:d annotations:p attribute_lists:p surfaces:p curves:p points:p key:p res_size:f res_linear:f instances:p next:p prev:p state:u owner:p body_kind:u geometry_state:u shell:p b_surfaces:p b_curves:p b_points:p region:p edge:p vertex:p",
        13 => "local_id:d annotations:p body:p next:p back_faces:p wire_edges:p vertex:p region:p front_faces:p",
        14 => "local_id:d annotations:p precision:f next_back:p prev_back:p loop:p back_shell:p surface:p sense:c next_sf:p prev_sf:p next_front:p prev_front:p front_shell:p",
        15 => "local_id:d annotations:p fin:p face:p next:p",
        16 => "local_id:d annotations:p precision:f fin:p prev:p next:p curve:p next_ce:p prev_ce:p owner:p",
        17 => "annotations:p loop:p forward:p backward:p vertex:p other:p edge:p curve:p next_vf:p sense:c",
        18 => "local_id:d annotations:p fin:p prev:p next:p point:p precision:f owner:p",
        19 => "local_id:d annotations:p body:p next:p prev:p shell:p kind:c",
        29 => "local_id:d annotations:p owner:p next:p prev:p pvec:v",
        40 => "base_parameter:f base_scale:f chart_count:d chordal_error:f angular_error:f parameter_error:f*2 hvec:h*1",
        41 => "type:c hvec:h*1",
        45 => "vertices:f*1",
        70 => "local_id:d owner:p next:p prev:p list_type:d length:d block_length:d size_of_entry:d finger_block:p finger_index_block:p finger_index:d notransmit:l",
        74 => "n_entries:d next_block:p entries:p*1",
        79 => "string:c*1",
        80 => "next:p identifier:p type:d actions:u*8 legal_owners:p fields:l*14 field_types:u*1",
        81 => "local_id:d definition:p owner:p next:p prev:p next_of_type:p prev_of_type:p fields:p*1",
        82 => "values:d*1",
        83 => "values:f*1",
        84 => "values:c*1",
        98 => "values:w*1",
        100 => "node_id:d owner:p next:p prev:p rotation:f*9 translation:v scale:f flag:d perspective:v",
        125 => "original_uint:i original_vint:i extended_uint:i extended_vint:i self_int:u original_u_start:c original_u_end:c original_v_start:c original_v_end:c extended_u_start:c extended_u_end:c extended_v_start:c extended_v_end:c analytic_form_type:c swept_form_type:c spun_form_type:c blend_form_type:c analytic_form:p swept_form:p spun_form:p blend_form:p",
        126 => "u_periodic:l v_periodic:l u_degree:n v_degree:n n_u_vertices:d n_v_vertices:d u_knot_type:u v_knot_type:u n_u_knots:d n_v_knots:d rational:l u_closed:l v_closed:l surface_form:u vertex_dim:n bspline_vertices:p u_knot_mult:p v_knot_mult:p u_knots:p v_knots:p",
        127 => "mult:n*1",
        128 => "knots:f*1",
        135 => "self_int:u analytic_form:p",
        136 => "degree:n n_vertices:d vertex_dim:n n_knots:d knot_type:u periodic:l closed:l rational:l curve_form:u bspline_vertices:p knot_mult:p knots:p",
        141 => "geometry:p next:p prev:p shared_geometry:p",
        _ => {
            let rest = match t {
                30 => "pvec:v direction:v",
                31 => "centre:v normal:v x_axis:v radius:f",
                32 => "centre:v normal:v x_axis:v major:f minor:f",
                38 => "surface:p*2 chart:p start:p end:p",
                50 => "pvec:v normal:v x_axis:v",
                51 => "pvec:v axis:v radius:f x_axis:v",
                52 => "pvec:v axis:v radius:f sin:f cos:f x_axis:v",
                53 => "centre:v radius:f axis:v x_axis:v",
                54 => "centre:v axis:v major:f minor:f x_axis:v",
                56 => "blend_type:c surface:p*2 spine:p range:f*2 thumb_weight:f*2 boundary:p*2 start:p end:p",
                59 => "boundary:n blend:p",
                60 => "check:c true_offset:l surface:p offset:f scale:f",
                67 => "section:p sweep:v scale:f",
                68 => "profile:p base:v axis:v start:v end:v start_param:f end_param:f x_axis:v scale:f",
                124 | 134 => "nurbs:p data:p",
                133 => "basis:p point_1:v point_2:v parm_1:f parm_2:f",
                137 => "surface:p b_curve:p original:p tolerance_to_original:f",
                _ => return None,
            };
            return Some(format!("{GEO}{rest}"));
        }
    };
    Some(s.to_string())
}

fn base(t: u16) -> Option<Vec<Field>> {
    let spec = base_spec(t)?;
    Some(
        spec.split_whitespace()
            .map(|f| {
                let (name, code) = f.split_once(':').unwrap_or((f, "d"));
                let (code, n) = code.split_once('*').map(|(c, n)| (c, n.parse().unwrap_or(0))).unwrap_or((code, 0));
                Field { name: name.into(), code: Code::of(code).unwrap_or(Code::Int), n, sent: true }
            })
            .collect(),
    )
}

/// The text after the header, with the 80-column lines joined back together.
struct Reader {
    d: Vec<u8>,
    p: usize,
    spaces: u8,
}

impl Reader {
    fn new(bytes: &[u8]) -> Result<Reader, String> {
        let start = match bytes.windows(15).position(|w| w == b"**END_OF_HEADER") {
            Some(i) => bytes[i..].iter().position(|&b| b == b'\n').map(|j| i + j + 1).unwrap_or(bytes.len()),
            None if bytes.first() == Some(&b'T') => 0,
            None => return Err("this is not a Parasolid text file".into()),
        };
        let mut d = Vec::with_capacity(bytes.len() - start);
        for line in bytes[start..].split(|&b| b == b'\n') {
            let mut l = line;
            while let Some((&last, rest)) = l.split_last() {
                if last == b'\r' || last == b' ' {
                    l = rest;
                } else {
                    break;
                }
            }
            d.extend_from_slice(l);
        }
        Ok(Reader { d, p: 0, spaces: 0 })
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        let near = String::from_utf8_lossy(&self.d[self.p.min(self.d.len())..(self.p + 40).min(self.d.len())]).to_string();
        Err(format!("the Parasolid file is damaged or of an unknown kind ({what} at character {}: \"{near}\")", self.p))
    }

    fn token(&mut self) -> Result<&[u8], String> {
        let s = self.p;
        while self.p < self.d.len() && self.d[self.p] != b' ' {
            self.p += 1;
        }
        if self.p == s {
            return self.err("missing value");
        }
        let e = self.p;
        if self.p < self.d.len() {
            self.p += 1;
        }
        Ok(&self.d[s..e])
    }

    fn int(&mut self) -> Result<i64, String> {
        let t = self.token()?;
        match std::str::from_utf8(t).ok().and_then(|s| s.parse().ok()) {
            Some(v) => Ok(v),
            None => {
                self.p -= t.len().min(self.p);
                self.err("expected a whole number")
            }
        }
    }

    /// A number, or NaN for a null ("?").
    fn num(&mut self) -> Result<f64, String> {
        if self.d.get(self.p) == Some(&b'?') {
            self.p += 1;
            return Ok(f64::NAN);
        }
        let t = self.token()?;
        match std::str::from_utf8(t).ok().and_then(|s| s.parse().ok()) {
            Some(v) => Ok(v),
            None => self.err("expected a number"),
        }
    }

    fn ch(&mut self) -> Result<u8, String> {
        if self.spaces > 0 {
            self.spaces -= 1;
            return Ok(b' ');
        }
        let Some(&c) = self.d.get(self.p) else { return self.err("unexpected end") };
        self.p += 1;
        if c != b'\\' {
            return Ok(c);
        }
        let Some(&e) = self.d.get(self.p) else { return self.err("unexpected end") };
        self.p += 1;
        Ok(match e {
            b'0' => 0,
            b'n' => b'\r',
            b'r' => b'\n',
            b'9' => {
                self.spaces = 8;
                b' '
            }
            _ => e,
        })
    }

    fn string(&mut self) -> Result<String, String> {
        let n = self.int()?;
        if !(0..=100_000).contains(&n) {
            return self.err("bad text length");
        }
        let mut s = Vec::with_capacity(n as usize);
        for _ in 0..n {
            s.push(self.ch()?);
        }
        Ok(String::from_utf8_lossy(&s).to_string())
    }

    /// A field definition inside an embedded schema: name, pointer class, count, type.
    fn field(&mut self) -> Result<Field, String> {
        let name = self.string()?;
        let class = self.int()?;
        let n = self.int()?;
        // a count of 0 (one value), 1 (a per-node count) or a small fixed array
        if !(0..=10_000).contains(&n) {
            return self.err("bad field count");
        }
        let n = n as u32;
        let code = if class != 0 {
            Code::Ptr
        } else {
            let c = self.string()?;
            match Code::of(&c) {
                Some(c) => c,
                None => return self.err(&format!("unknown field type '{c}'")),
            }
        };
        let sent = if n == 1 { self.ch()? != b'F' } else { true };
        Ok(Field { name, code, n, sent })
    }
}

struct Node {
    t: u16,
    fields: Rc<Vec<Field>>,
    vals: Vec<Vec<f64>>,
}

impl Node {
    fn get(&self, name: &str) -> &[f64] {
        self.fields.iter().position(|f| f.name == name).map(|i| &self.vals[i][..]).unwrap_or(&[])
    }

    fn num(&self, name: &str) -> f64 {
        self.get(name).first().copied().unwrap_or(f64::NAN)
    }

    fn ptr(&self, name: &str) -> u32 {
        let v = self.num(name);
        if v.is_finite() && v > 0.0 {
            v as u32
        } else {
            0
        }
    }

    fn ch(&self, name: &str) -> u8 {
        let v = self.num(name);
        if v.is_finite() {
            v as u8
        } else {
            0
        }
    }

    /// The node's sense (orientation) field: true for '+'.
    fn plus(&self, name: &str) -> bool {
        self.ch(name) != b'-'
    }

    fn v3(&self, name: &str) -> DVec3 {
        let v = self.get(name);
        if v.len() >= 3 {
            DVec3::new(v[0], v[1], v[2])
        } else {
            DVec3::NAN
        }
    }

    fn text(&self) -> String {
        let v = self.vals.first().map(|v| &v[..]).unwrap_or(&[]);
        let s: String = v.iter().map(|&c| char::from_u32(c as u32).unwrap_or('?')).collect();
        s.trim_end_matches('\0').to_string()
    }
}

/// A parsed .x_t file.
pub struct Xt {
    nodes: HashMap<u32, Node>,
}

impl Xt {
    pub fn parse(bytes: &[u8]) -> Result<Xt, String> {
        let mut r = Reader::new(bytes)?;
        if r.ch()? != b'T' {
            return Err("only Parasolid text files (.x_t) can be read, not binary ones (.x_b)".into());
        }
        let _modeller = r.string()?;
        let key = r.string()?;
        let parts: Vec<&str> = key.split('_').collect();
        let embedded = parts.len() == 4;
        let base_schema = parts.last().copied().unwrap_or("");
        if parts.first() != Some(&"SCH") || parts.len() < 3 {
            return r.err("no schema key");
        }
        if base_schema != "13006" {
            return Err(format!("this Parasolid file uses schema {base_schema}, which rCAD can't read yet (export it from a newer program, or as STEP)"));
        }
        if embedded {
            r.int()?;
        }
        if r.int()? != 0 {
            return Err("this Parasolid file has user fields, which rCAD can't read yet".into());
        }
        let mut schemas: HashMap<u16, Rc<Vec<Field>>> = HashMap::new();
        let mut nodes = HashMap::new();
        loop {
            let t = r.int()?;
            if t == 1 {
                break;
            }
            if !(2..=10000).contains(&t) {
                return r.err("bad node type");
            }
            let t = t as u16;
            if !schemas.contains_key(&t) {
                let fields = if embedded {
                    let marker = r.int()?;
                    match (marker, base(t)) {
                        (255, Some(b)) => b,
                        (255, None) => return Err(format!("this Parasolid file has a kind of data (type {t}) that rCAD can't read yet")),
                        (m, None) => {
                            let _name = r.string()?;
                            let _description = r.string()?;
                            (0..m).map(|_| r.field()).collect::<Result<Vec<_>, _>>()?
                        }
                        (_, Some(b)) => {
                            let mut out = vec![];
                            let mut i = 0;
                            loop {
                                match r.ch()? {
                                    b'Z' => break,
                                    b'C' => {
                                        match b.get(i) {
                                            Some(f) => out.push(f.clone()),
                                            None => return r.err("schema copies past the end"),
                                        }
                                        i += 1;
                                    }
                                    b'D' => i += 1,
                                    b'I' | b'A' => out.push(r.field()?),
                                    _ => return r.err("bad schema change"),
                                }
                            }
                            out
                        }
                    }
                } else {
                    base(t).ok_or_else(|| format!("this Parasolid file has a kind of data (type {t}) that rCAD can't read yet"))?
                };
                schemas.insert(t, Rc::new(fields));
            }
            let fields = schemas[&t].clone();
            let var = if fields.last().map(|f| f.n == 1).unwrap_or(false) { r.int()? } else { 0 };
            if !(0..=50_000_000).contains(&var) {
                return r.err("bad length");
            }
            let index = r.int()?;
            if var as usize > r.d.len() - r.p.min(r.d.len()) {
                return r.err("bad length");
            }
            let mut vals = Vec::with_capacity(fields.len());
            for f in fields.iter() {
                let count = if !f.sent {
                    0
                } else {
                    match f.n {
                        0 => 1,
                        1 => var as usize,
                        n => n as usize,
                    }
                };
                // every value takes at least a character, so a damaged count can't ask for more
                let mut v = Vec::with_capacity((count * f.code.width()).min(r.d.len() - r.p.min(r.d.len())));
                for _ in 0..count {
                    match f.code {
                        Code::Char | Code::Logical => v.push(r.ch()? as f64),
                        c if c.width() > 1 => {
                            if r.d.get(r.p) == Some(&b'?') {
                                r.p += 1;
                                v.extend(std::iter::repeat_n(f64::NAN, c.width()));
                            } else {
                                for _ in 0..c.width() {
                                    v.push(r.num()?);
                                }
                            }
                        }
                        _ => v.push(r.num()?),
                    }
                }
                vals.push(v);
            }
            nodes.insert(index as u32, Node { t, fields, vals });
        }
        Ok(Xt { nodes })
    }

    fn node(&self, i: u32) -> Option<&Node> {
        if i == 0 {
            None
        } else {
            self.nodes.get(&i)
        }
    }

    fn node_of(&self, i: u32, types: &[u16]) -> Result<&Node, String> {
        match self.node(i) {
            Some(n) if types.contains(&n.t) => Ok(n),
            Some(n) => Err(format!("node {i} is a type {} where a type {types:?} was expected", n.t)),
            None => Err(format!("node {i} is missing")),
        }
    }

    /// Follow a `next` chain from `first` (stopping at loops).
    fn chain(&self, first: u32, next: &str) -> Vec<u32> {
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut i = first;
        while let Some(n) = self.node(i) {
            if !seen.insert(i) {
                break;
            }
            out.push(i);
            i = n.ptr(next);
        }
        out
    }

    /// A text or number attribute on a node, by its definition's name (like SDL/TYSA_NAME).
    fn attribute(&self, owner: u32, names: &[&str]) -> Option<&Node> {
        let first = self.node(owner)?.ptr("annotations");
        for a in self.chain(first, "next") {
            let a = self.node(a)?;
            if a.t != 81 {
                continue;
            }
            let Some(def) = self.node(a.ptr("definition")) else { continue };
            let Some(id) = self.node(def.ptr("identifier")) else { continue };
            if names.contains(&id.text().as_str()) {
                return a.get("fields").first().and_then(|&f| self.node(f as u32));
            }
        }
        None
    }

    fn name_of(&self, i: u32) -> Option<String> {
        let n = self.attribute(i, &["SDL/TYSA_NAME"])?;
        let s = n.text().trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn colour_of(&self, i: u32) -> Option<[f64; 3]> {
        let n = self.attribute(i, &["SDL/TYSA_COLOUR_2", "SDL/TYSA_COLOUR"])?;
        let v = n.get("values");
        (v.len() >= 3 && v[..3].iter().all(|c| (0.0..=1.0).contains(c))).then(|| [v[0], v[1], v[2]])
    }

    /// Every body with where it sits, walking the assembly from its roots.
    fn placed_bodies(&self) -> Vec<(u32, Xf)> {
        let mut used = std::collections::HashSet::new();
        for n in self.nodes.values() {
            if n.t == 11 {
                used.insert(n.ptr("part"));
            }
        }
        // top-level assemblies, and bodies no assembly places (they sit where they are)
        let mut roots: Vec<u32> = self.nodes.iter().filter(|(i, n)| (n.t == 10 || n.t == 12) && !used.contains(i)).map(|(i, _)| *i).collect();
        roots.sort();
        let mut out = vec![];
        for r in roots {
            self.walk(r, Xf::IDENTITY, &mut out, &mut vec![]);
        }
        out
    }

    /// `path` holds the assemblies above `i`, so a damaged file whose assembly holds itself
    /// can't loop; the count limit stops a damaged file from placing bodies without end.
    fn walk(&self, i: u32, xf: Xf, out: &mut Vec<(u32, Xf)>, path: &mut Vec<u32>) {
        let Some(n) = self.node(i) else { return };
        if path.len() > 32 || path.contains(&i) || out.len() >= 100_000 {
            return;
        }
        match n.t {
            12 => out.push((i, xf)),
            10 => {
                path.push(i);
                for inst in self.chain(n.ptr("sub_instance"), "next_in_part") {
                    let Some(inst) = self.node(inst) else { continue };
                    let local = self.node(inst.ptr("transform")).map(Xf::of).unwrap_or(Xf::IDENTITY);
                    self.walk(inst.ptr("part"), xf.then(local), out, path);
                }
                path.pop();
            }
            _ => {}
        }
    }
}

/// A placement: `p' = m p + t` (in metres).
#[derive(Clone, Copy, Debug)]
struct Xf {
    m: DMat3,
    t: DVec3,
}

impl Xf {
    const IDENTITY: Xf = Xf { m: DMat3::IDENTITY, t: DVec3::ZERO };

    /// A TRANSFORM node: its 3x3 block is stored row by row.
    fn of(n: &Node) -> Xf {
        let r = n.get("rotation");
        if r.len() < 9 || r.iter().any(|v| !v.is_finite()) {
            return Xf::IDENTITY;
        }
        let rows = DMat3::from_cols_array(&[r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8]]).transpose();
        let s = n.num("scale");
        let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
        let t = n.v3("translation");
        Xf { m: rows * s, t: if t.is_finite() { t * s } else { DVec3::ZERO } }
    }

    /// `local` placed inside `self`.
    fn then(self, local: Xf) -> Xf {
        Xf { m: self.m * local.m, t: self.m * local.t + self.t }
    }
}

/// The parts of a converted file.
pub struct Converted {
    pub step: String,
    pub parts: Vec<String>,
    /// Faces that had geometry rCAD can't convert yet (left out of their part).
    pub skipped: usize,
}

/// Turn a Parasolid text file into a STEP file (millimetres), one product per placed body.
pub fn to_step(bytes: &[u8], title: &str) -> Result<Converted, String> {
    let xt = Xt::parse(bytes)?;
    let bodies = xt.placed_bodies();
    if bodies.is_empty() {
        return Err("the Parasolid file has no parts".into());
    }
    let mut s = Step::default();
    let ctx = s.header();
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut parts = vec![];
    let mut skipped = 0;
    let mut styled = vec![];
    for (k, (body, xf)) in bodies.iter().enumerate() {
        let base = xt.name_of(*body).unwrap_or_else(|| format!("Part {}", k + 1));
        let count = names.entry(base.clone()).or_insert(0);
        *count += 1;
        let name = if *count > 1 { format!("{base} {count}") } else { base };
        let mut c = Conv { xt: &xt, s: &mut s, xf: *xf, verts: HashMap::new(), edges: HashMap::new(), skipped: 0 };
        let items = c.body(*body)?;
        skipped += c.skipped;
        if items.is_empty() {
            continue;
        }
        if let Some(rgb) = xt.colour_of(*body) {
            for &(item, _) in &items {
                styled.push(s.style(item, rgb));
            }
        }
        s.product(&name, &items, ctx);
        parts.push(name);
    }
    if parts.is_empty() {
        return Err("none of the parts in the Parasolid file could be converted".into());
    }
    if !styled.is_empty() {
        let list = refs(&styled);
        s.add(format!("MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',({list}),#{ctx})"));
    }
    Ok(Converted { step: s.finish(title), parts, skipped })
}

/// Turns one placed body into STEP entities.
struct Conv<'a> {
    xt: &'a Xt,
    s: &'a mut Step,
    xf: Xf,
    verts: HashMap<u32, usize>,
    edges: HashMap<u32, usize>,
    skipped: usize,
}

const MM: f64 = 1000.0;

/// How deeply geometry may be built on other geometry (an offset of an offset, a trimmed
/// curve of a trimmed curve) before a file counts as damaged rather than looping for ever.
const NEST: usize = 8;

impl Conv<'_> {
    fn p(&self, v: DVec3) -> DVec3 {
        (self.xf.m * v + self.xf.t) * MM
    }

    fn d(&self, v: DVec3) -> DVec3 {
        (self.xf.m * v).normalize_or_zero()
    }

    fn len(&self, l: f64) -> f64 {
        l * MM * self.xf.m.determinant().abs().cbrt()
    }

    fn pt(&mut self, v: DVec3) -> usize {
        let p = self.p(v);
        self.s.point(p)
    }

    fn axis(&mut self, o: DVec3, z: DVec3, x: DVec3) -> usize {
        let (o, z, x) = (self.p(o), self.d(z), self.d(x));
        self.s.axis(o, z, x)
    }

    /// The body's shells as (STEP item, is a solid).
    fn body(&mut self, b: u32) -> Result<Vec<(usize, bool)>, String> {
        let xt = self.xt;
        let body = xt.node_of(b, &[12])?;
        let solid = body.num("body_kind") == 1.0;
        let mut items = vec![];
        let mut open_faces = vec![];
        // a sheet's faces have void on both sides, so each is in one shell's back faces and
        // in one shell's front faces: write it once
        let mut seen = std::collections::HashSet::new();
        for r in xt.chain(body.ptr("region"), "next") {
            let region = xt.node_of(r, &[19])?;
            let is_solid = region.ch("kind") == b'S';
            if solid && !is_solid {
                continue;
            }
            let mut shells = vec![];
            for sh in xt.chain(region.ptr("shell"), "next") {
                let shell = xt.node_of(sh, &[13])?;
                let mut faces = vec![];
                let mut pts = vec![];
                for (first, next, outward) in [("back_faces", "next_back", true), ("front_faces", "next_front", false)] {
                    for f in xt.chain(shell.ptr(first), next) {
                        if !solid && !seen.insert(f) {
                            continue;
                        }
                        match self.face(f, &mut pts) {
                            Ok(e) if outward || !solid => faces.push(e),
                            Ok(e) => faces.push(self.s.add(format!("ORIENTED_FACE('',*,#{e},.F.)"))),
                            Err(_) => self.skipped += 1,
                        }
                    }
                }
                if !faces.is_empty() {
                    shells.push((faces, pts));
                }
            }
            if !solid {
                for (f, _) in shells {
                    open_faces.extend(f);
                }
                continue;
            }
            // the shell spanning the most is the outside, any others are voids
            shells.sort_by(|a, b| span(&b.1).total_cmp(&span(&a.1)));
            let mut it = shells.into_iter();
            let Some((outer, _)) = it.next() else { continue };
            let outer = self.s.add(format!("CLOSED_SHELL('',({}))", refs(&outer)));
            let voids: Vec<usize> = it
                .map(|(f, _)| {
                    let cs = self.s.add(format!("CLOSED_SHELL('',({}))", refs(&f)));
                    self.s.add(format!("ORIENTED_CLOSED_SHELL('',*,#{cs},.F.)"))
                })
                .collect();
            let item =
                if voids.is_empty() { self.s.add(format!("MANIFOLD_SOLID_BREP('',#{outer})")) } else { self.s.add(format!("BREP_WITH_VOIDS('',#{outer},({}))", refs(&voids))) };
            items.push((item, true));
        }
        if !open_faces.is_empty() {
            let sh = self.s.add(format!("OPEN_SHELL('',({}))", refs(&open_faces)));
            items.push((self.s.add(format!("SHELL_BASED_SURFACE_MODEL('',(#{sh}))")), false));
        }
        Ok(items)
    }

    fn face(&mut self, f: u32, pts: &mut Vec<DVec3>) -> Result<usize, String> {
        let xt = self.xt;
        let face = xt.node_of(f, &[14])?;
        let blend = xt.node(face.ptr("surface")).filter(|n| n.t == 56);
        let (surf, surf_plus) = match blend {
            Some(b) => self.blend(face, b)?,
            None => self.surface(face.ptr("surface"))?,
        };
        let mut bounds = vec![];
        for l in xt.chain(face.ptr("loop"), "next") {
            let lp = xt.node_of(l, &[15])?;
            let mut oes = vec![];
            let mut ok = true;
            for fin in xt.chain(lp.ptr("fin"), "forward") {
                let fin = xt.node_of(fin, &[17])?;
                if fin.ptr("edge") == 0 {
                    ok = false;
                    break;
                }
                let e = self.edge(fin.ptr("edge"))?;
                if let Some(v) = xt.node(fin.ptr("vertex")).and_then(|v| xt.node(v.ptr("point"))) {
                    pts.push(self.p(v.v3("pvec")));
                }
                let sense = if fin.plus("sense") { ".T." } else { ".F." };
                oes.push(self.s.add(format!("ORIENTED_EDGE('',*,*,#{e},{sense})")));
            }
            // a loop of just a vertex (the tip of a cone) has no edges and needs no bound
            if ok && !oes.is_empty() {
                let el = self.s.add(format!("EDGE_LOOP('',({}))", refs(&oes)));
                bounds.push(self.s.add(format!("FACE_BOUND('',#{el},.T.)")));
            }
        }
        // a blend's surface is our own fit: its flag already says whether it faces the face's way
        let same = if blend.is_some() { surf_plus } else { face.plus("sense") == surf_plus };
        let same = if same { ".T." } else { ".F." };
        Ok(self.s.add(format!("ADVANCED_FACE('',({}),#{surf},{same})", refs(&bounds))))
    }

    /// A rolling-ball blend face: STEP has no such surface, so this fits a smooth B-spline through
    /// the ball's arcs between the two blended surfaces, over the stretch of spine the face covers.
    /// Returns the surface and whether its normal points the face's way.
    fn blend(&mut self, face: &Node, n: &Node) -> Result<(usize, bool), String> {
        let xt = self.xt;
        let range = n.get("range");
        let r = range.first().copied().unwrap_or(f64::NAN).abs();
        if !r.is_finite() || r < 1e-9 {
            return Err("blend without a radius".into());
        }
        let surfs = n.get("surface");
        let (s1, s2) = match surfs {
            [a, b, ..] => (xt.node(*a as u32).ok_or("blend without its surfaces")?, xt.node(*b as u32).ok_or("blend without its surfaces")?),
            _ => return Err("blend without its surfaces".into()),
        };
        // the face's corners and loop polygons (unplaced), for its stretch of spine and its side
        let mut loops: Vec<Vec<DVec3>> = vec![];
        for l in xt.chain(face.ptr("loop"), "next") {
            let lp = xt.node_of(l, &[15])?;
            let mut poly = vec![];
            for fin in xt.chain(lp.ptr("fin"), "forward") {
                let fin = xt.node_of(fin, &[17])?;
                if let Some(v) = xt.node(fin.ptr("vertex")).and_then(|v| xt.node(v.ptr("point"))) {
                    poly.push(v.v3("pvec"));
                }
            }
            loops.push(poly);
        }
        let corners: Vec<DVec3> = loops.iter().flatten().copied().collect();
        let spine = self.spine(n.ptr("spine"), &corners)?;
        let (nu, nv) = (spine.len(), 9);
        let mut grid = Vec::with_capacity(nu * nv);
        for c in &spine {
            let (a, b) = (self.foot(s1, *c)? - *c, self.foot(s2, *c)? - *c);
            let (la, lb) = (a.length(), b.length());
            let (ua, ub) = (a / la, b / lb);
            let th = ua.dot(ub).clamp(-1.0, 1.0).acos();
            if !(1e-6..std::f64::consts::PI - 1e-6).contains(&th) || (la - r).abs() > r * 0.5 || (lb - r).abs() > r * 0.5 {
                return Err("blend whose ball doesn't touch its surfaces".into());
            }
            for k in 0..nv {
                let t = k as f64 / (nv - 1) as f64;
                let d = (ua * ((1.0 - t) * th).sin() + ub * (t * th).sin()) / th.sin();
                grid.push(self.p(*c + d * (la + (lb - la) * t)));
            }
        }
        // the face's side: its outer loop runs anticlockwise seen from outside
        let at = |i: usize, j: usize| grid[i.min(nu - 1) * nv + j.min(nv - 1)];
        let (mi, mj) = (nu / 2, nv / 2);
        let normal = (at(mi + 1, mj) - at(mi.saturating_sub(1), mj)).cross(at(mi, mj + 1) - at(mi, mj.saturating_sub(1)));
        let outer = loops.iter().find(|l| l.len() >= 3).ok_or("blend face without corners")?;
        let pl: Vec<DVec3> = outer.iter().map(|p| self.p(*p)).collect();
        let mut newell = DVec3::ZERO;
        for i in 0..pl.len() {
            newell += pl[i].cross(pl[(i + 1) % pl.len()]);
        }
        let poles_v: Vec<Vec<DVec3>> = (0..nu).map(|i| catmull(&grid[i * nv..i * nv + nv])).collect();
        let pv = poles_v[0].len();
        let cols: Vec<Vec<DVec3>> = (0..pv).map(|j| catmull(&poles_v.iter().map(|row| row[j]).collect::<Vec<_>>())).collect();
        let pu = cols[0].len();
        let poles: Vec<DVec3> = (0..pu).flat_map(|i| cols.iter().map(move |c| c[i])).collect();
        let (um, uk) = catmull_knots(nu);
        let (vm, vk) = catmull_knots(nv);
        let e = self.s.bspline_surface(3, 3, pu, pv, &poles, &[], &um, &vm, &uk, &vk);
        Ok((e, normal.dot(newell) > 0.0))
    }

    /// The point of a surface a blend rests on (unplaced) nearest `p`: an analytic surface, or
    /// another blend (seen as the tube its ball sweeps).
    fn foot(&self, s: &Node, p: DVec3) -> Result<DVec3, String> {
        if s.t != 56 {
            return foot(s, p);
        }
        let r = s.get("range").first().copied().unwrap_or(f64::NAN).abs();
        let mut n = self.xt.node(s.ptr("spine")).ok_or("blend without a spine")?;
        if n.t == 133 {
            n = self.xt.node(n.ptr("basis")).ok_or("blend without a spine")?;
        }
        let q = if n.t == 30 {
            let (o, d) = (n.v3("pvec"), n.v3("direction").normalize_or_zero());
            o + d * (p - o).dot(d)
        } else {
            let poly = self.spine(s.ptr("spine"), &[])?;
            let mut best = (f64::INFINITY, poly[0]);
            for w in poly.windows(2) {
                let ab = w[1] - w[0];
                let t = if ab.length_squared() > 0.0 { ((p - w[0]).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                let q = w[0] + ab * t;
                if q.distance(p) < best.0 {
                    best = (q.distance(p), q);
                }
            }
            best.1
        };
        Ok(q + (p - q).try_normalize().ok_or("point on a blend's spine")? * r)
    }

    /// Ball centres (unplaced) along the stretch of a blend's spine next to `corners`.
    fn spine(&self, c: u32, corners: &[DVec3]) -> Result<Vec<DVec3>, String> {
        let xt = self.xt;
        let mut n = xt.node(c).ok_or("blend without a spine")?;
        if n.t == 133 {
            n = xt.node(n.ptr("basis")).ok_or("blend without a spine")?;
        }
        let (poly, closed): (Vec<DVec3>, bool) = match n.t {
            30 => {
                let (o, d) = (n.v3("pvec"), n.v3("direction").normalize_or_zero());
                let ts: Vec<f64> = corners.iter().map(|p| (*p - o).dot(d)).collect();
                if ts.len() < 2 {
                    return Err("straight blend without ends".into());
                }
                let (a, b) = (ts.iter().copied().fold(f64::INFINITY, f64::min), ts.iter().copied().fold(f64::NEG_INFINITY, f64::max));
                (vec![o + d * a, o + d * b], false)
            }
            31 | 32 => ((0..256).map(|i| self.curve_point(c, i as f64 / 256.0 * std::f64::consts::TAU, 0)).collect::<Result<_, _>>()?, true),
            38 => {
                let chart = xt.node_of(n.ptr("chart"), &[40])?;
                (chart.get("hvec").chunks(3).filter(|p| p.len() == 3).map(|p| DVec3::new(p[0], p[1], p[2])).collect(), false)
            }
            134 => {
                let b = self.bspline(n.ptr("nurbs"))?;
                ((0..=128).map(|i| b.eval(i as f64 / 128.0)).collect(), false)
            }
            t => return Err(format!("blend spine of type {t}")),
        };
        if poly.len() < 2 {
            return Err("blend spine without points".into());
        }
        // arc length along the polyline, and each corner's place on it
        let m = poly.len();
        let seg = if closed { m } else { m - 1 };
        let mut len = vec![0.0];
        for i in 0..seg {
            len.push(len[i] + poly[i].distance(poly[(i + 1) % m]));
        }
        let total = len[seg];
        let place = |p: DVec3| -> f64 {
            let mut best = (f64::INFINITY, 0.0);
            for i in 0..seg {
                let (a, b) = (poly[i], poly[(i + 1) % m]);
                let ab = b - a;
                let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                let d = p.distance(a + ab * t);
                if d < best.0 {
                    best = (d, len[i] + t * (len[i + 1] - len[i]));
                }
            }
            best.1
        };
        let mut ts: Vec<f64> = corners.iter().map(|p| place(*p)).collect();
        ts.sort_by(f64::total_cmp);
        let (a, b) = if ts.len() < 2 {
            // a ring all the way round (or a face with no corners): the whole spine
            (0.0, total)
        } else if closed {
            // the face covers the spine except its biggest gap between corners
            let mut gap = (total - ts[ts.len() - 1] + ts[0], ts[ts.len() - 1], ts[0] + total);
            for w in ts.windows(2) {
                if w[1] - w[0] > gap.0 {
                    gap = (w[1] - w[0], w[1], w[0] + total);
                }
            }
            (gap.1, gap.2)
        } else {
            (ts[0], ts[ts.len() - 1])
        };
        // run a little past the face so its edges lie well inside the surface
        let pad = (b - a) * 0.02 + 1e-7;
        let (a, b) = if closed { (a - pad, b + pad) } else { ((a - pad).max(0.0), (b + pad).min(total)) };
        let at = |s: f64| -> DVec3 {
            let s = if closed { s.rem_euclid(total) } else { s.clamp(0.0, total) };
            let i = len.partition_point(|&l| l <= s).clamp(1, seg) - 1;
            let span = len[i + 1] - len[i];
            let t = if span > 0.0 { (s - len[i]) / span } else { 0.0 };
            poly[i].lerp(poly[(i + 1) % m], t)
        };
        let k = if corners.is_empty() { 256 } else { 24 };
        Ok((0..=k).map(|i| at(a + (b - a) * i as f64 / k as f64)).collect())
    }

    fn vertex(&mut self, v: u32) -> Result<usize, String> {
        if let Some(&e) = self.verts.get(&v) {
            return Ok(e);
        }
        let xt = self.xt;
        let p = xt.node_of(xt.node_of(v, &[18])?.ptr("point"), &[29])?.v3("pvec");
        let pt = self.pt(p);
        let e = self.s.add(format!("VERTEX_POINT('',#{pt})"));
        self.verts.insert(v, e);
        Ok(e)
    }

    fn edge(&mut self, e: u32) -> Result<usize, String> {
        if let Some(&s) = self.edges.get(&e) {
            return Ok(s);
        }
        let xt = self.xt;
        let edge = xt.node_of(e, &[16])?;
        let fin = xt.node_of(edge.ptr("fin"), &[17])?;
        let other = xt.node(fin.ptr("other")).filter(|o| o.t == 17);
        // a fin ends at its vertex: the positive fin at the edge's end, the negative at its start
        let (mut start, mut end) = (0, 0);
        for f in std::iter::once(fin).chain(other) {
            if f.plus("sense") {
                end = f.ptr("vertex");
            } else {
                start = f.ptr("vertex");
            }
        }
        let mut cid = edge.ptr("curve");
        let mut fin_curve = false;
        if cid == 0 {
            // a tolerant edge has no curve of its own: each fin carries a curve in its face
            cid = std::iter::once(fin).chain(other).map(|f| f.ptr("curve")).find(|&c| c != 0).ok_or("edge without a curve")?;
            fin_curve = true;
        }
        let (curve, mut plus) = match self.curve(cid) {
            Ok(c) => c,
            Err(e) => {
                // a curve we can't follow (one lying on a blend, say): fall back on a fin's own curve
                let alt = std::iter::once(fin).chain(other).map(|f| f.ptr("curve")).filter(|&c| c != 0 && c != cid).find_map(|c| self.curve(c).ok().map(|r| (c, r)));
                let Some((c, r)) = alt else { return Err(e) };
                cid = c;
                fin_curve = true;
                r
            }
        };
        if fin_curve && start != end && start != 0 && end != 0 {
            // which way the fin's curve runs isn't the edge's: compare its points with the edge's ends
            let at = |v: u32| xt.node(v).and_then(|v| xt.node(v.ptr("point"))).map(|p| self.p(p.v3("pvec")));
            if let (Some(a), Some(b), Ok(pts)) = (at(start), at(end), self.sample(cid)) {
                let near = |q: DVec3| pts.iter().enumerate().min_by(|x, y| x.1.distance(q).total_cmp(&y.1.distance(q))).map(|(i, _)| i).unwrap_or(0);
                let (i, j) = (near(a), near(b));
                if i != j {
                    plus = i < j;
                }
            }
        }
        let (vs, ve) = match (start, end) {
            (0, 0) => {
                // a closed edge (a full circle) has no vertices in Parasolid but needs one in STEP
                let p = self.start_point(cid)?;
                let pt = self.s.point(p);
                let v = self.s.add(format!("VERTEX_POINT('',#{pt})"));
                (v, v)
            }
            (0, v) | (v, 0) => {
                let v = self.vertex(v)?;
                (v, v)
            }
            (a, b) => (self.vertex(a)?, self.vertex(b)?),
        };
        let same = if plus { ".T." } else { ".F." };
        let s = self.s.add(format!("EDGE_CURVE('',#{vs},#{ve},#{curve},{same})"));
        self.edges.insert(e, s);
        Ok(s)
    }

    /// A point on a closed curve (in mm), for the vertex STEP needs.
    fn start_point(&self, c: u32) -> Result<DVec3, String> {
        self.start_point_at(c, 0)
    }

    fn start_point_at(&self, c: u32, depth: usize) -> Result<DVec3, String> {
        if depth > NEST {
            return Err("curve built on itself".into());
        }
        let n = self.xt.node_of(c, &[30, 31, 32, 38, 133, 134, 137])?;
        Ok(match n.t {
            31 => self.p(n.v3("centre") + n.v3("x_axis").normalize_or_zero() * n.num("radius")),
            32 => self.p(n.v3("centre") + n.v3("x_axis").normalize_or_zero() * n.num("major")),
            133 => self.p(n.v3("point_1")),
            _ => {
                let pts = self.curve_points_at(c, depth)?;
                *pts.first().ok_or("empty curve")?
            }
        })
    }

    /// A curve's STEP entity and whether it runs the curve's own way.
    fn curve(&mut self, c: u32) -> Result<(usize, bool), String> {
        self.curve_at(c, 0)
    }

    fn curve_at(&mut self, c: u32, depth: usize) -> Result<(usize, bool), String> {
        if depth > NEST {
            return Err("curve built on itself".into());
        }
        let xt = self.xt;
        let n = xt.node_of(c, &[30, 31, 32, 38, 133, 134, 137])?;
        let plus = n.plus("orientation");
        let e = match n.t {
            30 => {
                let p = self.pt(n.v3("pvec"));
                let d = self.d(n.v3("direction"));
                let d = self.s.dir(d);
                let v = self.s.add(format!("VECTOR('',#{d},1.)"));
                self.s.add(format!("LINE('',#{p},#{v})"))
            }
            31 => {
                let a = self.axis(n.v3("centre"), n.v3("normal"), n.v3("x_axis"));
                let r = self.len(n.num("radius"));
                self.s.add(format!("CIRCLE('',#{a},{})", real(r)))
            }
            32 => {
                let a = self.axis(n.v3("centre"), n.v3("normal"), n.v3("x_axis"));
                let (ra, rb) = (self.len(n.num("major")), self.len(n.num("minor")));
                self.s.add(format!("ELLIPSE('',#{a},{},{})", real(ra), real(rb)))
            }
            133 => {
                let (e, p) = self.curve_at(n.ptr("basis"), depth + 1)?;
                return Ok((e, p == plus));
            }
            134 => {
                let b = self.bspline(n.ptr("nurbs"))?;
                let poles: Vec<DVec3> = b.poles.iter().map(|p| self.p(DVec3::new(p[0], p[1], p[2]))).collect();
                self.s.bspline_curve(b.degree, &poles, &b.weights, &b.mults, &b.knots)
            }
            _ => {
                // intersection and surface-parametric curves: a cubic through points along them
                let pts = self.curve_points_at(c, depth)?;
                self.s.fit(&pts)
            }
        };
        Ok((e, plus))
    }

    /// Points (in mm, in the curve's own direction) along an intersection or surface-parametric curve.
    fn curve_points_at(&self, c: u32, depth: usize) -> Result<Vec<DVec3>, String> {
        if depth > NEST {
            return Err("curve built on itself".into());
        }
        let xt = self.xt;
        let n = xt.node_of(c, &[38, 133, 134, 137])?;
        match n.t {
            38 => {
                let chart = xt.node_of(n.ptr("chart"), &[40])?;
                let h = chart.get("hvec");
                let pts: Vec<DVec3> = h.chunks(3).filter(|p| p.len() == 3).map(|p| self.p(DVec3::new(p[0], p[1], p[2]))).collect();
                if pts.len() < 2 {
                    return Err("intersection curve without points".into());
                }
                Ok(pts)
            }
            133 => self.curve_points_at(n.ptr("basis"), depth + 1),
            134 => {
                let b = self.bspline(n.ptr("nurbs"))?;
                Ok((0..=64).map(|i| self.p(b.eval(i as f64 / 64.0))).collect())
            }
            _ => {
                let b = self.bspline(xt.node_of(n.ptr("b_curve"), &[134])?.ptr("nurbs"))?;
                let sid = n.ptr("surface");
                let surf = xt.node_of(sid, &[50, 51, 52, 53, 54, 60, 67, 68, 124])?;
                // a straight line in a plane's parameters needs only its ends
                let steps = if surf.t == 50 && b.degree == 1 { b.poles.len() - 1 } else { 48 };
                if steps == 0 {
                    return Err("surface curve without length".into());
                }
                let mut out = vec![];
                for i in 0..=steps {
                    let uv = if surf.t == 50 && b.degree == 1 { DVec3::from_slice(&b.poles[i][..3]) } else { b.eval(i as f64 / steps as f64) };
                    out.push(self.p(self.surface_point(sid, uv.x, uv.y, depth + 1)?));
                }
                Ok(out)
            }
        }
    }

    /// Points (mm) along the STEP curve that `curve` writes for `c`, in that curve's own direction.
    fn sample(&self, c: u32) -> Result<Vec<DVec3>, String> {
        self.sample_at(c, 0)
    }

    fn sample_at(&self, c: u32, depth: usize) -> Result<Vec<DVec3>, String> {
        if depth > NEST {
            return Err("curve built on itself".into());
        }
        let n = self.xt.node_of(c, &[30, 31, 32, 38, 133, 134, 137])?;
        Ok(match n.t {
            30 => vec![self.p(n.v3("pvec")), self.p(n.v3("pvec") + n.v3("direction").normalize_or_zero() * 0.001)],
            31 | 32 => {
                let (o, z, x) = (n.v3("centre"), n.v3("normal").normalize_or_zero(), n.v3("x_axis").normalize_or_zero());
                let y = z.cross(x);
                let (ra, rb) = if n.t == 31 { (n.num("radius"), n.num("radius")) } else { (n.num("major"), n.num("minor")) };
                (0..64).map(|i| i as f64 / 64.0 * std::f64::consts::TAU).map(|t| self.p(o + x * (ra * t.cos()) + y * (rb * t.sin()))).collect()
            }
            133 => self.sample_at(n.ptr("basis"), depth + 1)?,
            _ => self.curve_points_at(c, depth)?,
        })
    }

    /// A point (in metres, unplaced) on a curve at its own Parasolid parameter.
    fn curve_point(&self, c: u32, t: f64, depth: usize) -> Result<DVec3, String> {
        if depth > NEST {
            return Err("curve built on itself".into());
        }
        let n = self.xt.node_of(c, &[30, 31, 32, 133, 134])?;
        Ok(match n.t {
            30 => n.v3("pvec") + n.v3("direction").normalize_or_zero() * t,
            31 | 32 => {
                let (o, z, x) = (n.v3("centre"), n.v3("normal").normalize_or_zero(), n.v3("x_axis").normalize_or_zero());
                let (ra, rb) = if n.t == 31 { (n.num("radius"), n.num("radius")) } else { (n.num("major"), n.num("minor")) };
                o + x * (ra * t.cos()) + z.cross(x) * (rb * t.sin())
            }
            133 => self.curve_point(n.ptr("basis"), t, depth + 1)?,
            _ => self.bspline(n.ptr("nurbs"))?.eval_at(t),
        })
    }

    /// A point (in metres, unplaced) on a surface at Parasolid parameters (u, v).
    fn surface_point(&self, s: u32, u: f64, v: f64, depth: usize) -> Result<DVec3, String> {
        if depth > NEST {
            return Err("surface built on itself".into());
        }
        let xt = self.xt;
        let n = xt.node(s).ok_or("missing surface")?;
        match n.t {
            50..=54 => surface_point(n, u, v),
            67 => {
                let c = self.curve_point(n.ptr("section"), u, depth + 1)?;
                Ok(c + n.v3("sweep") * v)
            }
            68 => {
                // the profile at u turned by v about the axis
                let p = self.curve_point(n.ptr("profile"), u, depth + 1)?;
                let (o, a) = (n.v3("base"), n.v3("axis").normalize_or_zero());
                let r = p - o;
                let along = a * a.dot(r);
                let out = r - along;
                Ok(o + along + out * v.cos() + a.cross(out) * v.sin())
            }
            60 => {
                let b = n.ptr("surface");
                let h = 1e-6;
                let p = self.surface_point(b, u, v, depth + 1)?;
                let du = self.surface_point(b, u + h, v, depth + 1)? - self.surface_point(b, u - h, v, depth + 1)?;
                let dv = self.surface_point(b, u, v + h, depth + 1)? - self.surface_point(b, u, v - h, depth + 1)?;
                let nrm = du.cross(dv).normalize_or_zero();
                let nrm = if xt.node(b).map(|b| b.plus("orientation")).unwrap_or(true) { nrm } else { -nrm };
                Ok(p + nrm * n.num("offset"))
            }
            124 => {
                let un = xt.node_of(n.ptr("nurbs"), &[126])?;
                let (ud, vd) = (un.num("u_degree") as usize, un.num("v_degree") as usize);
                let (nu, nv) = (un.num("n_u_vertices") as usize, un.num("n_v_vertices") as usize);
                let dim = un.num("vertex_dim") as usize;
                let rational = un.ch("rational") == b'T';
                let verts = xt.node_of(un.ptr("bspline_vertices"), &[45])?.get("vertices");
                let list = |p: &str, q: &str, count: &str| -> Result<(Vec<usize>, Vec<f64>), String> {
                    let nk = un.num(count);
                    let used = |v: &[f64]| -> Vec<f64> { if nk.is_finite() && (nk as usize) <= v.len() { v[..nk as usize].to_vec() } else { v.to_vec() } };
                    let m = used(xt.node_of(un.ptr(p), &[127])?.get("mult")).iter().map(|&m| m as usize).collect();
                    Ok((m, used(xt.node_of(un.ptr(q), &[128])?.get("knots"))))
                };
                let (um, uk) = list("u_knot_mult", "u_knots", "n_u_knots")?;
                let (vm, vk) = list("v_knot_mult", "v_knots", "n_v_knots")?;
                if verts.len() < nu * nv * dim {
                    return Err("B-spline surface without enough vertices".into());
                }
                let flat = |m: &[usize], k: &[f64]| -> Vec<f64> { k.iter().zip(m).flat_map(|(&k, &m)| std::iter::repeat_n(k, m)).collect() };
                let (fu, fv) = (flat(&um, &uk), flat(&vm, &vk));
                if fu.len() != nu + ud + 1 || fv.len() != nv + vd + 1 {
                    return Err("periodic or odd B-spline surface".into());
                }
                // each row (fixed u index) is a curve in v; then the row points make a curve in u,
                // in homogeneous coordinates so rational surfaces come out right
                let mut col = vec![];
                for i in 0..nu {
                    let row: Vec<[f64; 4]> = verts[i * nv * dim..(i + 1) * nv * dim]
                        .chunks(dim)
                        .map(|q| {
                            let w = if rational { q[dim - 1] } else { 1.0 };
                            let c = |k: usize| if k < dim - usize::from(rational) { q[k] } else { 0.0 };
                            // Parasolid stores rational vertices already multiplied by their weight
                            [c(0), c(1), c(2), w]
                        })
                        .collect();
                    col.push(deboor4(vd, &fv, &row, v));
                }
                let p = deboor4(ud, &fu, &col, u);
                Ok(DVec3::new(p[0], p[1], p[2]) / p[3])
            }
            t => Err(format!("curves on surface type {t} aren't supported yet")),
        }
    }

    fn bspline(&self, nurbs: u32) -> Result<Bspline, String> {
        let xt = self.xt;
        let n = xt.node_of(nurbs, &[136])?;
        let degree = n.num("degree") as usize;
        let dim = n.num("vertex_dim") as usize;
        let rational = n.ch("rational") == b'T';
        let verts = xt.node_of(n.ptr("bspline_vertices"), &[45])?.get("vertices");
        // the knot lists can be longer than the curve needs: n_knots says how much is used
        let nk = n.num("n_knots");
        let used = |v: &[f64]| -> Vec<f64> { if nk.is_finite() && (nk as usize) <= v.len() { v[..nk as usize].to_vec() } else { v.to_vec() } };
        let mults: Vec<usize> = used(xt.node_of(n.ptr("knot_mult"), &[127])?.get("mult")).iter().map(|&m| m as usize).collect();
        let knots = used(xt.node_of(n.ptr("knots"), &[128])?.get("knots"));
        let nv = n.num("n_vertices");
        let verts = if nv.is_finite() && (nv as usize) * dim <= verts.len() { &verts[..nv as usize * dim] } else { verts };
        Bspline::new(degree, dim, rational, verts, mults, knots)
    }

    /// A surface's STEP entity and whether its normal is the surface's own.
    fn surface(&mut self, s: u32) -> Result<(usize, bool), String> {
        self.surface_at(s, 0)
    }

    fn surface_at(&mut self, s: u32, depth: usize) -> Result<(usize, bool), String> {
        if depth > NEST {
            return Err("surface built on itself".into());
        }
        let xt = self.xt;
        let n = xt.node(s).ok_or("face without a surface")?;
        let plus = n.plus("orientation");
        let e = match n.t {
            50 => {
                let a = self.axis(n.v3("pvec"), n.v3("normal"), n.v3("x_axis"));
                self.s.add(format!("PLANE('',#{a})"))
            }
            51 => {
                let a = self.axis(n.v3("pvec"), n.v3("axis"), n.v3("x_axis"));
                let r = self.len(n.num("radius"));
                self.s.add(format!("CYLINDRICAL_SURFACE('',#{a},{})", real(r)))
            }
            52 => {
                // the radius grows along the axis when the half angle's sine is positive
                let (sin, cos) = (n.num("sin"), n.num("cos"));
                let axis = if sin < 0.0 { -n.v3("axis") } else { n.v3("axis") };
                let a = self.axis(n.v3("pvec"), axis, n.v3("x_axis"));
                let r = self.len(n.num("radius"));
                let angle = sin.abs().atan2(cos.abs());
                self.s.add(format!("CONICAL_SURFACE('',#{a},{},{})", real(r), real(angle)))
            }
            53 => {
                let a = self.axis(n.v3("centre"), n.v3("axis"), n.v3("x_axis"));
                let r = self.len(n.num("radius"));
                self.s.add(format!("SPHERICAL_SURFACE('',#{a},{})", real(r)))
            }
            54 => {
                let a = self.axis(n.v3("centre"), n.v3("axis"), n.v3("x_axis"));
                let (ra, rb) = (self.len(n.num("major")), self.len(n.num("minor")));
                self.s.add(format!("TOROIDAL_SURFACE('',#{a},{},{})", real(ra), real(rb)))
            }
            60 => {
                let (b, bp) = self.surface_at(n.ptr("surface"), depth + 1)?;
                let mut d = self.len(n.num("offset"));
                if !bp {
                    d = -d;
                }
                let e = self.s.add(format!("OFFSET_SURFACE('',#{b},{},.F.)", real(d)));
                return Ok((e, plus));
            }
            124 => {
                let un = xt.node_of(n.ptr("nurbs"), &[126])?;
                let (ud, vd) = (un.num("u_degree") as usize, un.num("v_degree") as usize);
                let (nu, nv) = (un.num("n_u_vertices") as usize, un.num("n_v_vertices") as usize);
                let dim = un.num("vertex_dim") as usize;
                let rational = un.ch("rational") == b'T';
                let verts = xt.node_of(un.ptr("bspline_vertices"), &[45])?.get("vertices");
                // the knot lists can be longer than the surface needs: n_u_knots and n_v_knots say how much is used
                let list = |p: &str, q: &str, count: &str| -> Result<(Vec<usize>, Vec<f64>), String> {
                    let nk = un.num(count);
                    let used = |v: &[f64]| -> Vec<f64> { if nk.is_finite() && (nk as usize) <= v.len() { v[..nk as usize].to_vec() } else { v.to_vec() } };
                    let m = used(xt.node_of(un.ptr(p), &[127])?.get("mult")).iter().map(|&m| m as usize).collect();
                    Ok((m, used(xt.node_of(un.ptr(q), &[128])?.get("knots"))))
                };
                let (um, uk) = list("u_knot_mult", "u_knots", "n_u_knots")?;
                let (vm, vk) = list("v_knot_mult", "v_knots", "n_v_knots")?;
                let w = if rational { dim.max(4) } else { dim.max(3) };
                let odd_mult = |m: &[usize], d: usize| m.iter().any(|&m| m == 0 || m > d + 1);
                if nu < 2
                    || nv < 2
                    || odd_mult(&um, ud)
                    || odd_mult(&vm, vd)
                    || um.len() != uk.len()
                    || vm.len() != vk.len()
                    || verts.len() < nu * nv * w
                    || um.iter().sum::<usize>() != nu + ud + 1
                    || vm.iter().sum::<usize>() != nv + vd + 1
                {
                    return Err("periodic or odd B-spline surface".into());
                }
                let mut poles = vec![];
                let mut weights = vec![];
                // the u index runs slowest, like STEP's list of rows
                for k in 0..nu * nv {
                    let v = &verts[k * w..k * w + w];
                    let wt = if rational { v[3] } else { 1.0 };
                    poles.push(self.p(DVec3::new(v[0], v[1], v[2]) / wt));
                    weights.push(wt);
                }
                self.s.bspline_surface(ud, vd, nu, nv, &poles, if rational { &weights } else { &[] }, &um, &vm, &uk, &vk)
            }
            67 => {
                // a section curve moved along a vector: STEP's linear extrusion
                let (c, _) = self.curve_at(n.ptr("section"), depth + 1)?;
                let sweep = n.v3("sweep");
                let len = self.len(sweep.length());
                let d = self.d(sweep);
                let d = self.s.dir(d);
                let v = self.s.add(format!("VECTOR('',#{d},{})", real(len)));
                self.s.add(format!("SURFACE_OF_LINEAR_EXTRUSION('',#{c},#{v})"))
            }
            68 => {
                // a profile curve spun about an axis: STEP's surface of revolution
                let (c, _) = self.curve_at(n.ptr("profile"), depth + 1)?;
                let o = self.pt(n.v3("base"));
                let a = self.d(n.v3("axis"));
                let a = self.s.dir(a);
                let ax = self.s.add(format!("AXIS1_PLACEMENT('',#{o},#{a})"));
                let e = self.s.add(format!("SURFACE_OF_REVOLUTION('',#{c},#{ax})"));
                // Parasolid runs u along the profile and v round the axis; STEP the other way
                // round, so its normal points the opposite way
                return Ok((e, !plus));
            }
            t => return Err(format!("surface type {t} isn't supported yet")),
        };
        Ok((e, plus))
    }
}

/// A point on an analytic surface at Parasolid parameters (u, v).
fn surface_point(s: &Node, u: f64, v: f64) -> Result<DVec3, String> {
    let frame = |o: DVec3, z: DVec3, x: DVec3| {
        let z = z.normalize_or_zero();
        let x = x.normalize_or_zero();
        (o, x, z.cross(x), z)
    };
    Ok(match s.t {
        50 => {
            let (o, x, y, _) = frame(s.v3("pvec"), s.v3("normal"), s.v3("x_axis"));
            o + x * u + y * v
        }
        51 => {
            let (o, x, y, z) = frame(s.v3("pvec"), s.v3("axis"), s.v3("x_axis"));
            o + (x * u.cos() + y * u.sin()) * s.num("radius") + z * v
        }
        52 => {
            let (o, x, y, z) = frame(s.v3("pvec"), s.v3("axis"), s.v3("x_axis"));
            let r = s.num("radius") + v * s.num("sin");
            o + (x * u.cos() + y * u.sin()) * r + z * v * s.num("cos")
        }
        53 => {
            let (o, x, y, z) = frame(s.v3("centre"), s.v3("axis"), s.v3("x_axis"));
            o + ((x * u.cos() + y * u.sin()) * v.cos() + z * v.sin()) * s.num("radius")
        }
        _ => {
            let (o, x, y, z) = frame(s.v3("centre"), s.v3("axis"), s.v3("x_axis"));
            let r = s.num("major") + s.num("minor") * v.cos();
            o + (x * u.cos() + y * u.sin()) * r + z * s.num("minor") * v.sin()
        }
    })
}

fn span(pts: &[DVec3]) -> f64 {
    let (lo, hi) = pts.iter().fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    if pts.is_empty() {
        0.0
    } else {
        (hi - lo).length()
    }
}

/// A B-spline curve from its Parasolid data: poles are 3D (x, y, z) or 2D (u, v) parameters.
struct Bspline {
    degree: usize,
    poles: Vec<[f64; 3]>,
    weights: Vec<f64>,
    mults: Vec<usize>,
    knots: Vec<f64>,
}

impl Bspline {
    fn new(degree: usize, dim: usize, rational: bool, verts: &[f64], mults: Vec<usize>, knots: Vec<f64>) -> Result<Bspline, String> {
        if dim == 0 || degree == 0 || mults.len() != knots.len() || verts.len() % dim != 0 || (rational && dim < 2) {
            return Err("odd B-spline curve".into());
        }
        let n = verts.len() / dim;
        if n < 2 || n <= degree || mults.iter().any(|&m| m == 0 || m > degree + 1) {
            return Err("B-spline curve with too few vertices".into());
        }
        if mults.iter().sum::<usize>() != n + degree + 1 {
            return Err("periodic B-spline curves aren't supported yet".into());
        }
        let space = if rational { dim - 1 } else { dim };
        let mut poles = vec![];
        let mut weights = vec![];
        for v in verts.chunks(dim) {
            let w = if rational { v[dim - 1] } else { 1.0 };
            let mut p = [0.0; 3];
            for k in 0..space.min(3) {
                p[k] = v[k] / w;
            }
            poles.push(p);
            weights.push(w);
        }
        if !rational {
            weights.clear();
        }
        Ok(Bspline { degree, poles, weights, mults, knots })
    }

    /// The point at `t` from 0 to 1 along the curve's knot range (de Boor).
    fn eval(&self, t: f64) -> DVec3 {
        let k = self.flat_knots();
        let p = self.degree;
        let (a, b) = (k[p], k[k.len() - p - 1]);
        self.eval_at(a + (b - a) * t.clamp(0.0, 1.0))
    }

    fn flat_knots(&self) -> Vec<f64> {
        self.knots.iter().zip(&self.mults).flat_map(|(&k, &m)| std::iter::repeat_n(k, m)).collect()
    }

    /// The point at the curve's own parameter `u` (held to its knot range).
    fn eval_at(&self, u: f64) -> DVec3 {
        let k = self.flat_knots();
        let p = self.degree;
        let u = u.clamp(k[p], k[k.len() - p - 1]);
        let mut s = p;
        while s + 1 < k.len() - p - 1 && k[s + 1] <= u {
            s += 1;
        }
        let w = |i: usize| self.weights.get(i).copied().unwrap_or(1.0);
        let mut d: Vec<(DVec3, f64)> = (0..=p)
            .map(|j| {
                let i = (s + j).saturating_sub(p).min(self.poles.len() - 1);
                (DVec3::from_array(self.poles[i]) * w(i), w(i))
            })
            .collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = s + j - p;
                let den = k[i + p + 1 - r] - k[i];
                let al = if den.abs() < 1e-300 { 0.0 } else { (u - k[i]) / den };
                d[j] = (d[j - 1].0 * (1.0 - al) + d[j].0 * al, d[j - 1].1 * (1.0 - al) + d[j].1 * al);
            }
        }
        d[p].0 / d[p].1
    }
}

/// The point of an analytic surface (unplaced) nearest `p`.
fn foot(s: &Node, p: DVec3) -> Result<DVec3, String> {
    Ok(match s.t {
        50 => {
            let n = s.v3("normal").normalize_or_zero();
            p - n * (p - s.v3("pvec")).dot(n)
        }
        51 | 52 => {
            let (o, a) = (s.v3("pvec"), s.v3("axis").normalize_or_zero());
            let h = (p - o).dot(a);
            let radial = p - o - a * h;
            let out = radial.try_normalize().ok_or("point on a surface's axis")?;
            if s.t == 51 {
                o + a * h + out * s.num("radius")
            } else {
                // the cone's line in the half plane through p: radius r + t·sin at height t·cos
                let (sin, cos) = (s.num("sin"), s.num("cos"));
                let base = o + out * s.num("radius");
                let dir = (out * sin + a * cos).normalize_or_zero();
                base + dir * (p - base).dot(dir)
            }
        }
        53 => {
            let c = s.v3("centre");
            c + (p - c).try_normalize().ok_or("point at a sphere's centre")? * s.num("radius")
        }
        54 => {
            let (c, a) = (s.v3("centre"), s.v3("axis").normalize_or_zero());
            let radial = (p - c) - a * (p - c).dot(a);
            let ring = c + radial.try_normalize().ok_or("point on a torus's axis")? * s.num("major");
            ring + (p - ring).try_normalize().ok_or("point on a torus's core")? * s.num("minor")
        }
        t => return Err(format!("blends on surface type {t} aren't supported yet")),
    })
}

/// Bezier poles of a smooth cubic through `pts` (Catmull-Rom tangents, one span per gap).
fn catmull(pts: &[DVec3]) -> Vec<DVec3> {
    let n = pts.len();
    let at = |i: isize| pts[i.clamp(0, n as isize - 1) as usize];
    let mut poles = vec![pts[0]];
    for i in 0..n as isize - 1 {
        let (a, b) = (at(i), at(i + 1));
        poles.push(a + (b - at(i - 1)) / 6.0);
        poles.push(b - (at(i + 2) - a) / 6.0);
        poles.push(b);
    }
    poles
}

/// The knots that go with `catmull`'s poles for `n` points.
fn catmull_knots(n: usize) -> (Vec<usize>, Vec<f64>) {
    let mut mults = vec![4];
    mults.extend(std::iter::repeat_n(3, n - 2));
    mults.push(4);
    (mults, (0..n).map(|i| i as f64).collect())
}

/// de Boor's algorithm on homogeneous points, at parameter `u` (held to the knot range).
fn deboor4(p: usize, k: &[f64], poles: &[[f64; 4]], u: f64) -> [f64; 4] {
    let u = u.clamp(k[p], k[k.len() - p - 1]);
    let mut s = p;
    while s + 1 < k.len() - p - 1 && k[s + 1] <= u {
        s += 1;
    }
    let mut d: Vec<[f64; 4]> = (0..=p).map(|j| poles[(s + j).saturating_sub(p).min(poles.len() - 1)]).collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = s + j - p;
            let den = k[i + p + 1 - r] - k[i];
            let al = if den.abs() < 1e-300 { 0.0 } else { (u - k[i]) / den };
            for c in 0..4 {
                d[j][c] = d[j - 1][c] * (1.0 - al) + d[j][c] * al;
            }
        }
    }
    d[p]
}

fn refs(ids: &[usize]) -> String {
    ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(",")
}

/// A STEP real: always with a decimal point, never an exponent.
fn real(v: f64) -> String {
    if !v.is_finite() || v.abs() < 1e-12 {
        return "0.".into();
    }
    let s = format!("{}", (v * 1e12).round() / 1e12);
    if s.contains('.') {
        s
    } else {
        s + "."
    }
}

fn text(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii() && c != '\\' { c } else { '_' }).collect::<String>().replace('\'', "''")
}

/// A STEP file being written.
#[derive(Default)]
struct Step {
    out: String,
    n: usize,
}

impl Step {
    fn add(&mut self, s: impl AsRef<str>) -> usize {
        self.n += 1;
        let _ = writeln!(self.out, "#{}={};", self.n, s.as_ref());
        self.n
    }

    fn point(&mut self, p: DVec3) -> usize {
        self.add(format!("CARTESIAN_POINT('',({},{},{}))", real(p.x), real(p.y), real(p.z)))
    }

    fn dir(&mut self, d: DVec3) -> usize {
        self.add(format!("DIRECTION('',({},{},{}))", real(d.x), real(d.y), real(d.z)))
    }

    fn axis(&mut self, o: DVec3, z: DVec3, x: DVec3) -> usize {
        let x = (x - z * x.dot(z)).normalize_or(z.any_orthonormal_vector());
        let (o, z, x) = (self.point(o), self.dir(z), self.dir(x));
        self.add(format!("AXIS2_PLACEMENT_3D('',#{o},#{z},#{x})"))
    }

    fn bspline_curve(&mut self, degree: usize, poles: &[DVec3], weights: &[f64], mults: &[usize], knots: &[f64]) -> usize {
        let ps: Vec<usize> = poles.iter().map(|p| self.point(*p)).collect();
        let m = mults.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(",");
        let k = knots.iter().map(|k| real(*k)).collect::<Vec<_>>().join(",");
        if weights.is_empty() {
            self.add(format!("B_SPLINE_CURVE_WITH_KNOTS('',{degree},({}),.UNSPECIFIED.,.F.,.F.,({m}),({k}),.UNSPECIFIED.)", refs(&ps)))
        } else {
            let w = weights.iter().map(|w| real(*w)).collect::<Vec<_>>().join(",");
            self.add(format!(
                "(BOUNDED_CURVE()B_SPLINE_CURVE({degree},({}),.UNSPECIFIED.,.F.,.F.)B_SPLINE_CURVE_WITH_KNOTS(({m}),({k}),.UNSPECIFIED.)CURVE()GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_CURVE(({w}))REPRESENTATION_ITEM(''))",
                refs(&ps)
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bspline_surface(&mut self, ud: usize, vd: usize, nu: usize, nv: usize, poles: &[DVec3], weights: &[f64], um: &[usize], vm: &[usize], uk: &[f64], vk: &[f64]) -> usize {
        let ps: Vec<usize> = poles.iter().map(|p| self.point(*p)).collect();
        let rows = (0..nu).map(|i| format!("({})", refs(&ps[i * nv..i * nv + nv]))).collect::<Vec<_>>().join(",");
        let ints = |v: &[usize]| v.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(",");
        let reals = |v: &[f64]| v.iter().map(|k| real(*k)).collect::<Vec<_>>().join(",");
        let (um, vm, uk, vk) = (ints(um), ints(vm), reals(uk), reals(vk));
        if weights.is_empty() {
            self.add(format!("B_SPLINE_SURFACE_WITH_KNOTS('',{ud},{vd},({rows}),.UNSPECIFIED.,.F.,.F.,.F.,({um}),({vm}),({uk}),({vk}),.UNSPECIFIED.)"))
        } else {
            let w = (0..nu).map(|i| format!("({})", reals(&weights[i * nv..i * nv + nv]))).collect::<Vec<_>>().join(",");
            self.add(format!(
                "(BOUNDED_SURFACE()B_SPLINE_SURFACE({ud},{vd},({rows}),.UNSPECIFIED.,.F.,.F.,.F.)B_SPLINE_SURFACE_WITH_KNOTS(({um}),({vm}),({uk}),({vk}),.UNSPECIFIED.)GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_SURFACE(({w}))REPRESENTATION_ITEM('')SURFACE())"
            ))
        }
    }

    /// A smooth cubic through the points (Catmull-Rom tangents, one span per gap).
    fn fit(&mut self, pts: &[DVec3]) -> usize {
        let n = pts.len();
        let at = |i: isize| pts[i.clamp(0, n as isize - 1) as usize];
        let mut poles = vec![pts[0]];
        for i in 0..n as isize - 1 {
            let (a, b) = (at(i), at(i + 1));
            poles.push(a + (b - at(i - 1)) / 6.0);
            poles.push(b - (at(i + 2) - a) / 6.0);
            poles.push(b);
        }
        let mut mults = vec![4];
        mults.extend(std::iter::repeat_n(3, n - 2));
        mults.push(4);
        let knots: Vec<f64> = (0..n).map(|i| i as f64).collect();
        self.bspline_curve(3, &poles, &[], &mults, &knots)
    }

    /// The units and contexts every product shares; returns the representation context.
    fn header(&mut self) -> usize {
        let app = self.add("APPLICATION_CONTEXT('core data for automotive mechanical design processes')");
        self.add(format!("APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"));
        let len = self.add("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))");
        let ang = self.add("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))");
        let sol = self.add("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())");
        let unc = self.add(format!("UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-04),#{len},'distance_accuracy_value','confusion accuracy')"));
        self.add(format!("(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{len},#{ang},#{sol}))REPRESENTATION_CONTEXT('Context3D','3D Context with 1E-04 uncertainty'))"))
    }

    fn product(&mut self, name: &str, items: &[(usize, bool)], ctx: usize) {
        let name = text(name);
        // contexts 1 (application) is shared; each product gets its own small chain
        let pc = self.add("PRODUCT_CONTEXT('',#1,'mechanical')");
        let p = self.add(format!("PRODUCT('{name}','{name}','',(#{pc}))"));
        let pdf = self.add(format!("PRODUCT_DEFINITION_FORMATION('','',#{p})"));
        let pdc = self.add("PRODUCT_DEFINITION_CONTEXT('part definition',#1,'design')");
        let pd = self.add(format!("PRODUCT_DEFINITION('design','',#{pdf},#{pdc})"));
        let pds = self.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
        let o = self.point(DVec3::ZERO);
        let (z, x) = (self.dir(DVec3::Z), self.dir(DVec3::X));
        let ax = self.add(format!("AXIS2_PLACEMENT_3D('',#{o},#{z},#{x})"));
        let solids: Vec<usize> = items.iter().filter(|i| i.1).map(|i| i.0).collect();
        let sheets: Vec<usize> = items.iter().filter(|i| !i.1).map(|i| i.0).collect();
        let mut reps = vec![];
        if !solids.is_empty() {
            reps.push(self.add(format!("ADVANCED_BREP_SHAPE_REPRESENTATION('{name}',(#{ax},{}),#{ctx})", refs(&solids))));
        }
        if !sheets.is_empty() {
            reps.push(self.add(format!("MANIFOLD_SURFACE_SHAPE_REPRESENTATION('{name}',(#{ax},{}),#{ctx})", refs(&sheets))));
        }
        let first = reps[0];
        self.add(format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{first})"));
        for &r in &reps[1..] {
            self.add(format!("SHAPE_REPRESENTATION_RELATIONSHIP('','',#{first},#{r})"));
        }
    }

    fn style(&mut self, item: usize, rgb: [f64; 3]) -> usize {
        let c = self.add(format!("COLOUR_RGB('',{},{},{})", real(rgb[0]), real(rgb[1]), real(rgb[2])));
        let fc = self.add(format!("FILL_AREA_STYLE_COLOUR('',#{c})"));
        let fa = self.add(format!("FILL_AREA_STYLE('',(#{fc}))"));
        let sf = self.add(format!("SURFACE_STYLE_FILL_AREA(#{fa})"));
        let ss = self.add(format!("SURFACE_SIDE_STYLE('',(#{sf}))"));
        let su = self.add(format!("SURFACE_STYLE_USAGE(.BOTH.,#{ss})"));
        let pa = self.add(format!("PRESENTATION_STYLE_ASSIGNMENT((#{su}))"));
        self.add(format!("STYLED_ITEM('color',(#{pa}),#{item})"))
    }

    fn finish(self, title: &str) -> String {
        let t = text(title);
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('{t} (from Parasolid)'),'2;1');\nFILE_NAME('{t}','',(''),(''),'rCAD','rCAD','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN {{ 1 0 10303 214 1 1 1 1 }}'));\nENDSEC;\nDATA;\n{}ENDSEC;\nEND-ISO-10303-21;\n",
            self.out
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a .x_t in the plain 13006 schema, node by node, filling unnamed fields with zeros.
    struct Writer {
        out: String,
    }

    impl Writer {
        fn node(&mut self, t: u16, i: u32, vals: &[(&str, &str)]) {
            let fields = base(t).unwrap();
            let _ = write!(self.out, "{t} {i} ");
            for f in fields.iter() {
                let v = vals.iter().find(|(n, _)| *n == f.name).map(|(_, v)| v.to_string());
                // a fixed array can be given whole, as numbers with spaces between
                let whole = f.code != Code::Vector && v.as_deref().is_some_and(|v| v.contains(' '));
                let count = if f.n == 0 || whole { 1 } else { f.n as usize };
                for _ in 0..count {
                    match f.code {
                        Code::Char => self.out.push_str(v.as_deref().unwrap_or("+")),
                        Code::Logical => self.out.push_str(v.as_deref().unwrap_or("F")),
                        // a null is a lone '?' with no space after it
                        Code::Vector => match &v {
                            Some(v) => {
                                let _ = write!(self.out, "{v} ");
                            }
                            None => self.out.push('?'),
                        },
                        _ => {
                            let _ = write!(self.out, "{} ", v.clone().unwrap_or("0".into()));
                        }
                    }
                }
            }
        }
    }

    /// A 10 mm cube (0..0.01 m) as one solid body, with a name.
    pub(crate) fn cube() -> String {
        boxes(&[([0.0; 3], 0.01, false)], false)
    }

    /// Axis-aligned boxes (corner, size in metres, is a cavity) as the shells of one body named
    /// Cube: a solid whose cavities are inner shells of its solid region, or with `sheet` a sheet
    /// body whose faces have the one (void) shell on both sides.
    fn boxes(list: &[([f64; 3], f64, bool)], sheet: bool) -> String {
        let mut w = Writer { out: String::from("**END_OF_HEADER*****\nT4 TEST17 SCH_1300000_130060 ") };
        // nodes: body 1, regions 2 (void) 3 (solid), shells 4..; per box (o = 1000 k) faces o+10..,
        // loops o+20.., planes o+30.., edges o+40.., lines o+60.., vertices o+80.., points o+90.., fins o+100..
        let kind = if sheet { "3" } else { "1" };
        w.node(12, 1, &[("body_kind", kind), ("region", "2"), ("annotations", "200")]);
        w.node(19, 2, &[("body", "1"), ("next", if sheet { "0" } else { "3" }), ("shell", if sheet { "4" } else { "0" }), ("kind", "V")]);
        if !sheet {
            w.node(19, 3, &[("body", "1"), ("prev", "2"), ("shell", "4"), ("kind", "S")]);
        }
        for (bk, &(origin, c, cavity)) in list.iter().enumerate() {
            let o = 1000 * bk;
            let corner = |k: usize| [origin[0] + (k & 1) as f64 * c, origin[1] + ((k >> 1) & 1) as f64 * c, origin[2] + ((k >> 2) & 1) as f64 * c];
            let mut faces: [(usize, [usize; 4]); 6] = [
                // outward normal axis, and corners counter-clockwise seen from outside
                (0, [0, 4, 6, 2]), // x = 0, normal -x
                (1, [1, 3, 7, 5]), // x = c
                (2, [0, 1, 5, 4]), // y = 0
                (3, [2, 6, 7, 3]), // y = c
                (4, [0, 2, 3, 1]), // z = 0
                (5, [4, 5, 7, 6]), // z = c
            ];
            if cavity {
                // the material is outside: normals point into the box, loops turn the other way
                for f in faces.iter_mut() {
                    f.1.reverse();
                }
            }
            let mut edges: Vec<(usize, usize)> = vec![];
            let mut fins = vec![];
            for (fi, (_, ring)) in faces.iter().enumerate() {
                for k in 0..4 {
                    let (a, b) = (ring[k], ring[(k + 1) % 4]);
                    let (e, sense) = match edges.iter().position(|&(x, y)| (x, y) == (b, a)) {
                        Some(e) => (e, '-'),
                        None => {
                            edges.push((a, b));
                            (edges.len() - 1, '+')
                        }
                    };
                    fins.push((fi, k, e, sense, b));
                }
            }
            let id = |n: usize| (o + n).to_string();
            let next_shell = if bk + 1 < list.len() { (5 + bk).to_string() } else { "0".into() };
            let region = if sheet { "2" } else { "3" };
            let mut shell = vec![("region", region.to_string()), ("back_faces", id(10)), ("next", next_shell)];
            if sheet {
                shell.push(("front_faces", id(10)));
            }
            let shell: Vec<(&str, &str)> = shell.iter().map(|(a, b)| (*a, b.as_str())).collect();
            w.node(13, 4 + bk as u32, &shell);
            for (fi, (axis, ring)) in faces.iter().enumerate() {
                let next = if fi < 5 { id(10 + fi + 1) } else { "0".into() };
                let (s, l, sh) = (id(30 + fi), id(20 + fi), (4 + bk).to_string());
                let mut v = vec![("next_back", next.as_str()), ("loop", &l), ("back_shell", &sh), ("surface", &s), ("sense", "+")];
                if sheet {
                    v.extend([("next_front", next.as_str()), ("front_shell", &sh)]);
                }
                w.node(14, (o + 10 + fi) as u32, &v);
                w.node(15, (o + 20 + fi) as u32, &[("fin", &id(100 + fi * 4)), ("face", &id(10 + fi))]);
                let mut n = [0.0; 3];
                n[axis / 2] = if (axis % 2 == 1) != cavity { 1.0 } else { -1.0 };
                let p = corner(ring[0]);
                let x = if axis / 2 == 0 { "0 1 0" } else { "1 0 0" };
                w.node(50, (o + 30 + fi) as u32, &[("pvec", &format!("{} {} {}", p[0], p[1], p[2])), ("normal", &format!("{} {} {}", n[0], n[1], n[2])), ("x_axis", x)]);
            }
            for (k, &(fi, i, e, sense, vend)) in fins.iter().enumerate() {
                let fwd = o + 100 + fi * 4 + (i + 1) % 4;
                let bwd = o + 100 + fi * 4 + (i + 3) % 4;
                let other = fins.iter().position(|f| f.2 == e && f.3 != sense).unwrap() + o + 100;
                let s = sense.to_string();
                let vals = [
                    ("loop", id(20 + fi)),
                    ("forward", fwd.to_string()),
                    ("backward", bwd.to_string()),
                    ("vertex", id(80 + vend)),
                    ("other", other.to_string()),
                    ("edge", id(40 + e)),
                ];
                let mut v: Vec<(&str, &str)> = vals.iter().map(|(a, b)| (*a, b.as_str())).collect();
                v.push(("sense", &s));
                w.node(17, (o + 100 + k) as u32, &v);
            }
            for (e, &(a, b)) in edges.iter().enumerate() {
                let fin = fins.iter().position(|f| f.2 == e && f.3 == '+').unwrap() + o + 100;
                w.node(16, (o + 40 + e) as u32, &[("fin", &fin.to_string()), ("curve", &id(60 + e)), ("owner", "1")]);
                let (pa, pb) = (corner(a), corner(b));
                let d = [(pb[0] - pa[0]) / c, (pb[1] - pa[1]) / c, (pb[2] - pa[2]) / c];
                w.node(30, (o + 60 + e) as u32, &[("pvec", &format!("{} {} {}", pa[0], pa[1], pa[2])), ("direction", &format!("{} {} {}", d[0], d[1], d[2]))]);
            }
            for k in 0..8 {
                let p = corner(k);
                w.node(18, (o + 80 + k) as u32, &[("point", &id(90 + k)), ("owner", "1")]);
                w.node(29, (o + 90 + k) as u32, &[("pvec", &format!("{} {} {}", p[0], p[1], p[2]))]);
            }
        }
        name_and_wrap(w)
    }

    /// Ends a file: names node 1 "Cube" (attribute 200), adds the terminator and wraps at 80 columns.
    fn name_and_wrap(mut w: Writer) -> String {
        // its name: an attribute (81) of definition 201 named by 202, holding text 203
        w.out.push_str("81 1 200 0 201 1 0 0 0 0 203 ");
        w.out.push_str("80 1 201 0 202 8017 0 0 0 0 0 0 0 0 0 FFFFFFFFFFFFFF3 ");
        w.out.push_str("79 13 202 SDL/TYSA_NAME");
        w.out.push_str("84 4 203 Cube");
        w.out.push_str("1 0");
        // wrap at 80 columns like real files
        let s = w.out;
        let mut wrapped = String::new();
        for (i, line) in s.split('\n').enumerate() {
            if i == 0 {
                wrapped.push_str(line);
                wrapped.push('\n');
                continue;
            }
            // a line never ends in a space (readers trim them), so the space starts the next line
            let mut rest = line;
            while !rest.is_empty() {
                let mut n = rest.len().min(80);
                while n > 1 && rest.as_bytes()[n - 1] == b' ' {
                    n -= 1;
                }
                wrapped.push_str(&rest[..n]);
                wrapped.push('\n');
                rest = &rest[n..];
            }
        }
        wrapped
    }

    /// The STEP read back by OpenCascade the way an import is: (valid, volume in mm³).
    fn occ_read(step: &str, tag: &str) -> (bool, f64) {
        let path = std::env::temp_dir().join(format!("rcad-parasolid-{tag}-{}.step", std::process::id()));
        std::fs::write(&path, step).unwrap();
        let s = opencascade::primitives::Shape::read_step(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        (crate::occ::is_valid(&s), crate::kernel::volume(&s))
    }

    #[test]
    fn a_sheet_body_has_each_face_once() {
        let c = to_step(boxes(&[([0.0; 3], 0.01, false)], true).as_bytes(), "sheet").unwrap();
        assert_eq!(c.step.matches("ADVANCED_FACE").count(), 6, "{}", c.step);
        assert!(c.step.contains("OPEN_SHELL") && !c.step.contains("CLOSED_SHELL"));
    }

    #[test]
    fn a_cavity_is_left_out_of_the_volume() {
        // a 10 mm cube with a 2 mm cube of nothing inside it
        let x = boxes(&[([0.0; 3], 0.01, false), ([0.004; 3], 0.002, true)], false);
        let c = to_step(x.as_bytes(), "hollow").unwrap();
        assert!(c.step.contains("BREP_WITH_VOIDS"));
        let (valid, vol) = occ_read(&c.step, "hollow");
        assert!(valid);
        assert!((vol - 992.0).abs() < 1e-3, "volume {vol}");
    }

    /// A ball: one face on a sphere, with no loops at all.
    fn ball() -> String {
        let mut w = Writer { out: String::from("**END_OF_HEADER*****\nT4 TEST17 SCH_1300000_130060 ") };
        w.node(12, 1, &[("body_kind", "1"), ("region", "2"), ("annotations", "200")]);
        w.node(19, 2, &[("body", "1"), ("next", "3"), ("kind", "V")]);
        w.node(19, 3, &[("body", "1"), ("prev", "2"), ("shell", "4"), ("kind", "S")]);
        w.node(13, 4, &[("region", "3"), ("back_faces", "10")]);
        w.node(14, 10, &[("back_shell", "4"), ("surface", "30"), ("sense", "+")]);
        w.node(53, 30, &[("centre", "0 0 0"), ("radius", "0.005"), ("axis", "0 0 1"), ("x_axis", "1 0 0")]);
        name_and_wrap(w)
    }

    #[test]
    fn a_face_without_loops_reads_back() {
        let c = to_step(ball().as_bytes(), "ball").unwrap();
        // the face has no bounds at all; OpenCascade gives it the sphere's natural ones
        assert!(c.step.contains("ADVANCED_FACE('',(),"));
        let (valid, vol) = occ_read(&c.step, "ball");
        assert!(valid);
        let want = 4.0 / 3.0 * std::f64::consts::PI * 125.0;
        assert!((vol - want).abs() < 0.01 * want, "volume {vol}, want {want}");
    }

    /// Nodes in the 13006 schema without the cube's name, ended and wrapped.
    fn file(nodes: impl FnOnce(&mut Writer)) -> String {
        let mut w = Writer { out: String::from("**END_OF_HEADER*****\nT4 TEST17 SCH_1300000_130060 ") };
        nodes(&mut w);
        w.out.push_str("1 0");
        w.out
    }

    #[test]
    fn bodies_outside_the_assembly_are_kept() {
        // assembly 2 places body 5 (moved 1 m in x); body 6 isn't in any assembly
        let x = file(|w| {
            w.node(10, 2, &[("sub_instance", "3")]);
            w.node(11, 3, &[("part", "5"), ("assembly", "2"), ("transform", "4")]);
            w.node(100, 4, &[("rotation", "1 0 0 0 1 0 0 0 1"), ("translation", "1 0 0"), ("scale", "1")]);
            w.node(12, 5, &[("body_kind", "1")]);
            w.node(12, 6, &[("body_kind", "1")]);
        });
        let xt = Xt::parse(x.as_bytes()).unwrap();
        let b = xt.placed_bodies();
        assert_eq!(b.iter().map(|b| b.0).collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(b[0].1.t, DVec3::X);
        assert_eq!(b[1].1.t, DVec3::ZERO);
    }

    #[test]
    fn damaged_files_give_errors_not_crashes() {
        // assembly 2 (placed by root 1) holds itself and body 6
        let x = file(|w| {
            w.node(10, 1, &[("sub_instance", "5")]);
            w.node(11, 5, &[("part", "2")]);
            w.node(10, 2, &[("sub_instance", "3")]);
            w.node(11, 3, &[("part", "2"), ("next_in_part", "4")]);
            w.node(11, 4, &[("part", "6")]);
            w.node(12, 6, &[("body_kind", "1")]);
        });
        let xt = Xt::parse(x.as_bytes()).unwrap();
        assert_eq!(xt.placed_bodies().iter().map(|b| b.0).collect::<Vec<_>>(), vec![6]);
        // an offset surface of itself, a trimmed curve of itself
        let x = file(|w| {
            w.node(60, 7, &[("surface", "7"), ("offset", "0.001")]);
            w.node(133, 8, &[("basis", "8")]);
        });
        let xt = Xt::parse(x.as_bytes()).unwrap();
        let mut s = Step::default();
        let mut c = Conv { xt: &xt, s: &mut s, xf: Xf::IDENTITY, verts: HashMap::new(), edges: HashMap::new(), skipped: 0 };
        assert!(c.surface(7).is_err());
        assert!(c.curve(8).is_err());
        let _ = c.start_point(8);
        // B-spline vertices that don't fill whole points, or too few of them
        assert!(Bspline::new(1, 4, true, &[0.0; 10], vec![2, 2], vec![0.0, 1.0]).is_err());
        assert!(Bspline::new(1, 3, false, &[0.0; 3], vec![1, 1], vec![0.0, 1.0]).is_err());
        assert!(Bspline::new(2, 3, false, &[0.0; 9], vec![2, 4], vec![0.0, 1.0]).is_err());
        // a length far past the end of the file
        assert!(Xt::parse(b"**END_OF_HEADER\nT4 test17 SCH_1300000_130060 84 40000000 5 abc").is_err());
        // an embedded schema field with a negative count
        assert!(Xt::parse(b"**END_OF_HEADER\nT4 test23 SCH_3801231_38101_13006249 0 29 6 CCCCDCA5 extra0 -1 1 dZ7 1 0 0 0 1 2 3 42 1 0").is_err());
    }

    #[test]
    fn reads_a_cube_and_writes_step() {
        let x = cube();
        let xt = Xt::parse(x.as_bytes()).unwrap();
        assert_eq!(xt.name_of(1).as_deref(), Some("Cube"));
        let c = to_step(x.as_bytes(), "cube").unwrap();
        assert_eq!(c.parts, vec!["Cube"]);
        assert_eq!(c.skipped, 0);
        assert_eq!(c.step.matches("ADVANCED_FACE").count(), 6);
        assert_eq!(c.step.matches("EDGE_CURVE").count(), 12);
        assert_eq!(c.step.matches("VERTEX_POINT").count(), 8);
        assert_eq!(c.step.matches("ORIENTED_EDGE").count(), 24);
        assert!(c.step.contains("CARTESIAN_POINT('',(10.,10.,10.))"), "in millimetres");
    }

    #[test]
    fn embedded_schema_changes() {
        // a type-29 point whose schema drops `prev` and adds a number at the end
        let s = "**END_OF_HEADER\nT4 test23 SCH_3801231_38101_13006249 0 29 6 CCCCDCA5 extra0 0 1 dZ7 1 0 0 0 1 2 3 42 1 0";
        let xt = Xt::parse(s.as_bytes()).unwrap();
        let p = xt.node(7).unwrap();
        assert_eq!(p.v3("pvec"), DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(p.num("extra"), 42.0);
        assert!(p.get("prev").is_empty());
    }

    #[test]
    #[ignore]
    fn convert_file() {
        // PARASOLID=in.x_t: writes in.x_t.step next to it
        let path = std::env::var("PARASOLID").unwrap();
        let c = to_step(&std::fs::read(&path).unwrap(), "probe").unwrap();
        eprintln!("parts {:?} skipped faces {}", c.parts, c.skipped);
        std::fs::write(format!("{path}.step"), c.step).unwrap();
    }

    #[test]
    fn blend_fit_and_feet() {
        // the Catmull-Rom poles make a cubic B-spline through every point, at whole-number knots
        let pts = [DVec3::ZERO, DVec3::new(1.0, 2.0, 0.0), DVec3::new(3.0, 2.5, 1.0), DVec3::new(4.0, 0.0, 0.0)];
        let poles: Vec<f64> = catmull(&pts).iter().flat_map(|p| p.to_array()).collect();
        let (m, k) = catmull_knots(pts.len());
        let b = Bspline::new(3, 3, false, &poles, m, k).unwrap();
        for (i, p) in pts.iter().enumerate() {
            assert!((b.eval_at(i as f64) - *p).length() < 1e-12);
        }
        // a bilinear patch in homogeneous points
        let row = |y: f64| vec![[0.0, y, 0.0, 1.0], [2.0, y, 0.0, 1.0]];
        let col = [deboor4(1, &[0.0, 0.0, 1.0, 1.0], &row(0.0), 0.25), deboor4(1, &[0.0, 0.0, 1.0, 1.0], &row(4.0), 0.25)];
        let p = deboor4(1, &[0.0, 0.0, 1.0, 1.0], &col, 0.5);
        assert_eq!(p, [0.5, 2.0, 0.0, 1.0]);
        // the feet of a ball centre on a cylinder and a torus
        let mut x = Writer { out: String::new() };
        x.node(51, 1, &[("pvec", "0 0 0"), ("axis", "0 0 1"), ("radius", "2"), ("x_axis", "1 0 0")]);
        x.node(54, 2, &[("centre", "0 0 0"), ("axis", "0 0 1"), ("major", "5"), ("minor", "1"), ("x_axis", "1 0 0")]);
        let mut r = Reader { d: x.out.into_bytes(), p: 0, spaces: 0 };
        let mut node = |t: u16| {
            let fields = Rc::new(base(t).unwrap());
            r.int().unwrap();
            r.int().unwrap();
            let vals = fields
                .iter()
                .map(|f| {
                    let n = if f.n == 0 { 1 } else { f.n as usize } * f.code.width();
                    (0..n).map(|_| if matches!(f.code, Code::Char | Code::Logical) { r.ch().map(|c| c as f64) } else { r.num() }).collect::<Result<Vec<_>, _>>().unwrap()
                })
                .collect();
            Node { t, fields, vals }
        };
        let (cyl, tor) = (node(51), node(54));
        assert!((foot(&cyl, DVec3::new(5.0, 0.0, 3.0)).unwrap() - DVec3::new(2.0, 0.0, 3.0)).length() < 1e-12);
        assert!((foot(&tor, DVec3::new(0.0, 7.0, 0.0)).unwrap() - DVec3::new(0.0, 6.0, 0.0)).length() < 1e-12);
        // swept surfaces are known to the reader
        assert!(base(67).is_some());
    }

    #[test]
    fn numbers_and_text() {
        assert_eq!(real(1.0), "1.");
        assert_eq!(real(-0.5), "-0.5");
        assert_eq!(real(1e-14), "0.");
        assert_eq!(text("it's"), "it''s");
        let b = Bspline::new(1, 2, false, &[0.0, 0.0, 2.0, 4.0], vec![2, 2], vec![0.0, 1.0]).unwrap();
        assert!((b.eval(0.5) - DVec3::new(1.0, 2.0, 0.0)).length() < 1e-12);
        assert!(Xt::parse(b"hello").is_err());
    }
}
