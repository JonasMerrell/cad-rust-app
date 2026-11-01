//! Small expression evaluator for dimension and feature values, e.g. `width / 2`,
//! `25.4 mm`, `2 * pi * r`, `30 deg`.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[default]
    Mm,
    Cm,
    M,
    In,
    Ft,
}

impl Units {
    pub const ALL: [Units; 5] = [Units::Mm, Units::Cm, Units::M, Units::In, Units::Ft];

    /// Millimetres per unit.
    pub fn scale(self) -> f64 {
        match self {
            Units::Mm => 1.0,
            Units::Cm => 10.0,
            Units::M => 1000.0,
            Units::In => 25.4,
            Units::Ft => 304.8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Units::Mm => "mm",
            Units::Cm => "cm",
            Units::M => "m",
            Units::In => "in",
            Units::Ft => "ft",
        }
    }

    /// Format a length given in mm.
    pub fn fmt(self, mm: f64) -> String {
        trim(mm / self.scale(), 3)
    }

    pub fn fmt_with(self, mm: f64) -> String {
        format!("{} {}", self.fmt(mm), self.label())
    }
}

pub fn trim(v: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, v);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// What a value means, so plain numbers get the right unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Length,
    Angle,
    Count,
}

/// What a value carries: nothing (a plain number, read in the document's units in a length
/// field), a length in mm, or an angle in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dim {
    None,
    Len,
    Ang,
}

/// Resolved document variables. Derefs to the values as a length field reads them (mm, or degrees
/// for angles); a variable written as a plain number ("4", "2 * 15") is also kept as that number,
/// so a count or angle field reads it as it is written whatever the document's units.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    vals: HashMap<String, f64>,
    dims: HashMap<String, (f64, Dim)>,
}

impl std::ops::Deref for Vars {
    type Target = HashMap<String, f64>;
    fn deref(&self) -> &HashMap<String, f64> {
        &self.vals
    }
}

/// Values already in mm or degrees, each carrying its unit.
impl From<HashMap<String, f64>> for Vars {
    fn from(vals: HashMap<String, f64>) -> Vars {
        let dims = vals.iter().map(|(k, v)| (k.clone(), (*v, Dim::Len))).collect();
        Vars { vals, dims }
    }
}

impl Vars {
    fn lookup(&self, name: &str) -> Option<V> {
        self.dims.get(name).map(|&(v, dim)| V { v, dim })
    }
}

pub struct Env<'a> {
    pub vars: &'a Vars,
    pub units: Units,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(char),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() || ch == '.' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') && i + 1 < c.len() && (c[i + 1].is_ascii_digit() || c[i + 1] == '-' || c[i + 1] == '+') {
                i += 2;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t}"))?));
        } else if ch.is_alphabetic() || ch == '_' || ch == '#' {
            let st = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect()));
        } else if ch == '°' {
            out.push(Tok::Id("deg".into()));
            i += 1;
        } else if ch == '"' {
            out.push(Tok::Id("in".into()));
            i += 1;
        } else if "+-*/^(),%".contains(ch) {
            out.push(Tok::Op(ch));
            i += 1;
        } else {
            return Err(format!("Unexpected character: {ch}"));
        }
    }
    Ok(out)
}

/// A value with what it carries, so `10` in a length field means 10 document units but `10 mm`
/// is explicit, and `30 deg` stays degrees inside `sin`.
#[derive(Clone, Copy)]
struct V {
    v: f64,
    dim: Dim,
}

struct P<'a, 'b> {
    t: Vec<Tok>,
    i: usize,
    env: &'a Env<'b>,
    kind: Kind,
    /// Units that plain numbers are read in: the document's, or the unit written after a
    /// parenthesised group, as in `(w + 1) mm`.
    units: Units,
}

impl P<'_, '_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    /// Plain numbers added to (or compared with) a length are lengths in the current units, so
    /// `w + 1` in an inch document adds an inch. Other fields (counts, angles) leave them alone.
    fn unify(&self, a: V, b: V) -> (V, V, Dim) {
        let k = if self.kind == Kind::Length { self.units.scale() } else { 1.0 };
        match (a.dim, b.dim) {
            (Dim::None, Dim::Len) => (V { v: a.v * k, dim: Dim::Len }, b, Dim::Len),
            (Dim::Len, Dim::None) => (a, V { v: b.v * k, dim: Dim::Len }, Dim::Len),
            (Dim::None, d) => (a, b, d),
            (d, _) => (a, b, d),
        }
    }
    fn expr(&mut self) -> Result<V, String> {
        let mut a = self.term()?;
        loop {
            if self.eat('+') {
                let b = self.term()?;
                let (x, y, dim) = self.unify(a, b);
                a = V { v: x.v + y.v, dim };
            } else if self.eat('-') {
                let b = self.term()?;
                let (x, y, dim) = self.unify(a, b);
                a = V { v: x.v - y.v, dim };
            } else {
                return Ok(a);
            }
        }
    }
    fn term(&mut self) -> Result<V, String> {
        let mut a = self.unary()?;
        loop {
            if self.eat('*') {
                let b = self.unary()?;
                a = V { v: a.v * b.v, dim: if a.dim == Dim::None { b.dim } else { a.dim } };
            } else if self.eat('/') {
                let b = self.unary()?;
                if b.v == 0.0 {
                    return Err("division by zero".into());
                }
                // a length over a length is a ratio
                a = V { v: a.v / b.v, dim: if b.dim == Dim::None { a.dim } else { Dim::None } };
            } else if self.eat('%') {
                let b = self.unary()?;
                let (x, y, dim) = self.unify(a, b);
                if y.v == 0.0 {
                    return Err("division by zero".into());
                }
                a = V { v: x.v % y.v, dim };
            } else {
                return Ok(a);
            }
        }
    }
    fn unary(&mut self) -> Result<V, String> {
        if self.eat('-') {
            let a = self.unary()?;
            return Ok(V { v: -a.v, dim: a.dim });
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }
    fn power(&mut self) -> Result<V, String> {
        let a = self.postfix()?;
        if self.eat('^') {
            let b = self.unary()?;
            return Ok(V { v: a.v.powf(b.v), dim: a.dim });
        }
        Ok(a)
    }
    /// A primary optionally followed by a unit suffix (`10 mm`, `30deg`). A value that already
    /// carries a unit (a variable written with one) keeps it.
    fn postfix(&mut self) -> Result<V, String> {
        let mut a = self.primary()?;
        if let Some(Tok::Id(id)) = self.peek().cloned() {
            if let Some((f, dim)) = unit_factor(&id) {
                self.i += 1;
                if a.dim == Dim::None {
                    a = V { v: a.v * f, dim };
                }
            }
        }
        Ok(a)
    }
    /// The length unit written right after the group that opens at token `open`, if any.
    fn group_unit(&self, open: usize) -> Option<Units> {
        let mut depth = 0;
        for k in open..self.t.len() {
            match self.t[k] {
                Tok::Op('(') => depth += 1,
                Tok::Op(')') => {
                    depth -= 1;
                    if depth == 0 {
                        return match self.t.get(k + 1) {
                            Some(Tok::Id(id)) => length_unit(id),
                            _ => None,
                        };
                    }
                }
                _ => {}
            }
        }
        None
    }
    fn primary(&mut self) -> Result<V, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(V { v: n, dim: Dim::None })
            }
            Some(Tok::Op('(')) => {
                let outer = self.units;
                if let Some(u) = self.group_unit(self.i) {
                    self.units = u;
                }
                self.i += 1;
                let v = self.expr();
                self.units = outer;
                let v = v?;
                if !self.eat(')') {
                    return Err("missing )".into());
                }
                Ok(v)
            }
            Some(Tok::Id(id)) => {
                self.i += 1;
                if self.eat('(') {
                    let mut args = vec![self.expr()?];
                    while self.eat(',') {
                        args.push(self.expr()?);
                    }
                    if !self.eat(')') {
                        return Err("missing )".into());
                    }
                    return self.call(&id, &args);
                }
                // document variables come before the built-in constants, so a variable may be called `e`
                if let Some(v) = self.env.vars.lookup(&id) {
                    return Ok(v);
                }
                match id.as_str() {
                    "pi" | "PI" => return Ok(V { v: std::f64::consts::PI, dim: Dim::None }),
                    "e" => return Ok(V { v: std::f64::consts::E, dim: Dim::None }),
                    _ => {}
                }
                Err(format!("unknown variable '{id}'"))
            }
            _ => Err("expected a value".into()),
        }
    }
    fn call(&mut self, f: &str, a: &[V]) -> Result<V, String> {
        let x = a[0].v;
        // angles are kept in degrees (`rad` converts to them), and plain numbers are degrees too
        let plain = |v: f64| V { v, dim: Dim::None };
        let same = |v: f64| V { v, dim: a[0].dim };
        Ok(match (f, a.len()) {
            ("sin", 1) => plain(x.to_radians().sin()),
            ("cos", 1) => plain(x.to_radians().cos()),
            ("tan", 1) => plain(x.to_radians().tan()),
            ("asin", 1) => plain(x.asin().to_degrees()),
            ("acos", 1) => plain(x.acos().to_degrees()),
            ("atan", 1) => plain(x.atan().to_degrees()),
            ("sqrt", 1) => same(x.sqrt()),
            ("abs", 1) => same(x.abs()),
            ("round", 1) => same(x.round()),
            ("floor", 1) => same(x.floor()),
            ("ceil", 1) => same(x.ceil()),
            ("min", _) | ("max", _) => {
                let mut acc = a[0];
                for b in &a[1..] {
                    let (p, q, dim) = self.unify(acc, *b);
                    acc = V { v: if f == "min" { p.v.min(q.v) } else { p.v.max(q.v) }, dim };
                }
                acc
            }
            _ => return Err(format!("unknown function {f}")),
        })
    }
}

fn unit_factor(id: &str) -> Option<(f64, Dim)> {
    if let Some(u) = length_unit(id) {
        return Some((u.scale(), Dim::Len));
    }
    Some(match id {
        "deg" => (1.0, Dim::Ang),
        "rad" => (180.0 / std::f64::consts::PI, Dim::Ang),
        _ => return None,
    })
}

fn length_unit(id: &str) -> Option<Units> {
    Some(match id {
        "mm" => Units::Mm,
        "cm" => Units::Cm,
        "m" => Units::M,
        "in" | "inch" => Units::In,
        "ft" => Units::Ft,
        _ => return None,
    })
}

fn eval_v(src: &str, kind: Kind, env: &Env) -> Result<V, String> {
    let t = lex(src)?;
    if t.is_empty() {
        return Err("empty".into());
    }
    let mut p = P { t, i: 0, env, kind, units: env.units };
    let v = p.expr()?;
    if p.i != p.t.len() {
        return Err("unexpected text after value".into());
    }
    if !v.v.is_finite() {
        return Err("not a number".into());
    }
    Ok(v)
}

/// Evaluate `src`. Lengths come back in mm, angles in degrees.
pub fn eval(src: &str, kind: Kind, env: &Env) -> Result<f64, String> {
    let v = eval_v(src, kind, env)?;
    Ok(match kind {
        Kind::Length if v.dim == Dim::None => v.v * env.units.scale(),
        _ => v.v,
    })
}

/// Evaluate document variables in order; later ones may reference earlier ones. A variable written
/// as a plain number keeps that number for count and angle fields, and is read in the document's
/// units by length fields.
pub fn resolve_vars(vars: &[(String, String)], units: Units) -> (Vars, Vec<Option<String>>) {
    let mut map = Vars::default();
    let mut errs = vec![];
    for (name, src) in vars {
        let env = Env { vars: &map, units };
        match eval_v(src, Kind::Length, &env) {
            Ok(v) => {
                map.vals.insert(name.clone(), if v.dim == Dim::None { v.v * units.scale() } else { v.v });
                map.dims.insert(name.clone(), (v.v, v.dim));
                errs.push(None);
            }
            Err(e) => errs.push(Some(e)),
        }
    }
    (map, errs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics() {
        let mut vars = HashMap::new();
        vars.insert("width".to_string(), 40.0);
        let vars = Vars::from(vars);
        let env = Env { vars: &vars, units: Units::Mm };
        assert_eq!(eval("width / 2", Kind::Length, &env).unwrap(), 20.0);
        assert_eq!(eval("1 in", Kind::Length, &env).unwrap(), 25.4);
        assert_eq!(eval("2*(3+4)", Kind::Count, &env).unwrap(), 14.0);
        let env = Env { vars: &vars, units: Units::In };
        assert_eq!(eval("2", Kind::Length, &env).unwrap(), 50.8);
        assert_eq!(eval("10 mm", Kind::Length, &env).unwrap(), 10.0);
        assert!(eval("foo", Kind::Length, &env).is_err());
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * b.abs().max(1.0)
    }

    fn doc(units: Units, vars: &[(&str, &str)]) -> Vars {
        let pairs: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let (v, errs) = resolve_vars(&pairs, units);
        assert!(errs.iter().all(|e| e.is_none()), "{errs:?}");
        v
    }

    #[test]
    fn trig_takes_degrees_with_or_without_a_unit() {
        let vars = doc(Units::Mm, &[("a", "30"), ("b", "30 deg"), ("c", "(pi / 6) rad")]);
        let env = Env { vars: &vars, units: Units::Mm };
        for src in ["sin(30)", "sin(30 deg)", "sin(30°)", "sin((pi / 6) rad)", "sin(a)", "sin(b)", "sin(c)", "sin(a + 0 deg)"] {
            assert!(close(eval(src, Kind::Count, &env).unwrap(), 0.5), "{src}");
        }
        assert!(close(eval("10 * cos(a)", Kind::Length, &env).unwrap(), 10.0 * 3f64.sqrt() / 2.0));
        assert!(close(eval("asin(0.5)", Kind::Angle, &env).unwrap(), 30.0));
        assert!(close(eval("1 rad", Kind::Angle, &env).unwrap(), 180.0 / std::f64::consts::PI));
    }

    #[test]
    fn mm_documents_read_as_before() {
        let vars = doc(Units::Mm, &[("w", "40"), ("h", "w / 2"), ("n", "4"), ("ang", "30")]);
        let env = Env { vars: &vars, units: Units::Mm };
        assert_eq!(vars["w"], 40.0);
        assert_eq!(vars["h"], 20.0);
        assert_eq!(eval("w + 1", Kind::Length, &env).unwrap(), 41.0);
        assert_eq!(eval("w + 1 in", Kind::Length, &env).unwrap(), 65.4);
        assert_eq!(eval("max(w, 50)", Kind::Length, &env).unwrap(), 50.0);
        assert_eq!(eval("n", Kind::Count, &env).unwrap(), 4.0);
        assert_eq!(eval("n * 2 + 1", Kind::Count, &env).unwrap(), 9.0);
        assert_eq!(eval("ang", Kind::Angle, &env).unwrap(), 30.0);
        assert_eq!(eval("(2 * 4) mm", Kind::Length, &env).unwrap(), 8.0);
    }

    #[test]
    fn inch_documents_read_plain_numbers_in_inches() {
        let vars = doc(Units::In, &[("w", "2"), ("n", "4"), ("ang", "30"), ("m", "10 mm"), ("e", "3")]);
        let env = Env { vars: &vars, units: Units::In };
        // as a length a plain variable is in inches; as a count or an angle it is the number written
        assert!(close(vars["w"], 50.8));
        assert!(close(eval("w", Kind::Length, &env).unwrap(), 50.8));
        assert_eq!(eval("n", Kind::Count, &env).unwrap(), 4.0);
        assert_eq!(eval("n + 1", Kind::Count, &env).unwrap(), 5.0);
        assert_eq!(eval("ang", Kind::Angle, &env).unwrap(), 30.0);
        assert!(close(eval("sin(ang)", Kind::Count, &env).unwrap(), 0.5));
        // plain numbers mixed with lengths are inches too
        assert!(close(eval("w + 1", Kind::Length, &env).unwrap(), 76.2));
        assert!(close(eval("1 in + 1", Kind::Length, &env).unwrap(), 50.8));
        assert!(close(eval("m + 1", Kind::Length, &env).unwrap(), 35.4));
        assert!(close(eval("max(m, 1)", Kind::Length, &env).unwrap(), 25.4));
        assert!(close(eval("m * 2", Kind::Length, &env).unwrap(), 20.0));
        assert!(close(eval("m / 2 mm", Kind::Count, &env).unwrap(), 5.0));
        assert!(close(eval("10 mm", Kind::Length, &env).unwrap(), 10.0));
        // a unit after a group reads the plain numbers inside it in that unit
        assert!(close(eval("(m + 1) mm", Kind::Length, &env).unwrap(), 11.0));
        // a variable may be called e
        assert_eq!(eval("e", Kind::Count, &env).unwrap(), 3.0);
    }
}
