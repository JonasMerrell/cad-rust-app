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

pub struct Env<'a> {
    pub vars: &'a HashMap<String, f64>,
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

/// A value carries whether a unit was attached, so `10` in a length field means
/// 10 document units but `10 mm` is explicit.
#[derive(Clone, Copy)]
struct V {
    v: f64,
    dim: bool,
}

struct P<'a, 'b> {
    t: Vec<Tok>,
    i: usize,
    env: &'a Env<'b>,
    kind: Kind,
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
    fn expr(&mut self) -> Result<V, String> {
        let mut a = self.term()?;
        loop {
            if self.eat('+') {
                let b = self.term()?;
                a = V { v: a.v + b.v, dim: a.dim || b.dim };
            } else if self.eat('-') {
                let b = self.term()?;
                a = V { v: a.v - b.v, dim: a.dim || b.dim };
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
                a = V { v: a.v * b.v, dim: a.dim || b.dim };
            } else if self.eat('/') {
                let b = self.unary()?;
                if b.v == 0.0 {
                    return Err("division by zero".into());
                }
                a = V { v: a.v / b.v, dim: a.dim && !b.dim };
            } else if self.eat('%') {
                let b = self.unary()?;
                a = V { v: a.v % b.v, dim: a.dim };
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
    /// A primary optionally followed by a unit suffix (`10 mm`, `30deg`).
    fn postfix(&mut self) -> Result<V, String> {
        let mut a = self.primary()?;
        if let Some(Tok::Id(id)) = self.peek().cloned() {
            if let Some(f) = unit_factor(&id) {
                self.i += 1;
                a = V { v: a.v * f, dim: true };
            }
        }
        Ok(a)
    }
    fn primary(&mut self) -> Result<V, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(V { v: n, dim: false })
            }
            Some(Tok::Op('(')) => {
                self.i += 1;
                let v = self.expr()?;
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
                match id.as_str() {
                    "pi" | "PI" => return Ok(V { v: std::f64::consts::PI, dim: false }),
                    "e" => return Ok(V { v: std::f64::consts::E, dim: false }),
                    _ => {}
                }
                if let Some(v) = self.env.vars.get(&id) {
                    // Variables are stored already resolved (mm / degrees).
                    return Ok(V { v: *v, dim: true });
                }
                Err(format!("unknown variable '{id}'"))
            }
            _ => Err("expected a value".into()),
        }
    }
    fn call(&mut self, f: &str, a: &[V]) -> Result<V, String> {
        let x = a[0].v;
        let trig_in = |v: V| if v.dim { v.v } else { v.v.to_radians() };
        let r = match (f, a.len()) {
            ("sin", 1) => trig_in(a[0]).sin(),
            ("cos", 1) => trig_in(a[0]).cos(),
            ("tan", 1) => trig_in(a[0]).tan(),
            ("asin", 1) => return Ok(V { v: x.asin().to_degrees(), dim: false }),
            ("acos", 1) => return Ok(V { v: x.acos().to_degrees(), dim: false }),
            ("atan", 1) => return Ok(V { v: x.atan().to_degrees(), dim: false }),
            ("sqrt", 1) => return Ok(V { v: x.sqrt(), dim: a[0].dim }),
            ("abs", 1) => return Ok(V { v: x.abs(), dim: a[0].dim }),
            ("round", 1) => return Ok(V { v: x.round(), dim: a[0].dim }),
            ("floor", 1) => return Ok(V { v: x.floor(), dim: a[0].dim }),
            ("ceil", 1) => return Ok(V { v: x.ceil(), dim: a[0].dim }),
            ("min", _) => return Ok(V { v: a.iter().map(|v| v.v).fold(f64::INFINITY, f64::min), dim: a[0].dim }),
            ("max", _) => return Ok(V { v: a.iter().map(|v| v.v).fold(f64::NEG_INFINITY, f64::max), dim: a[0].dim }),
            _ => return Err(format!("unknown function {f}")),
        };
        Ok(V { v: r, dim: false })
    }
}

fn unit_factor(id: &str) -> Option<f64> {
    Some(match id {
        "mm" => 1.0,
        "cm" => 10.0,
        "m" => 1000.0,
        "in" | "inch" => 25.4,
        "ft" => 304.8,
        "deg" => 1.0,
        "rad" => 180.0 / std::f64::consts::PI,
        _ => return None,
    })
}

/// Evaluate `src`. Lengths come back in mm, angles in degrees.
pub fn eval(src: &str, kind: Kind, env: &Env) -> Result<f64, String> {
    let t = lex(src)?;
    if t.is_empty() {
        return Err("empty".into());
    }
    let mut p = P { t, i: 0, env, kind };
    let v = p.expr()?;
    if p.i != p.t.len() {
        return Err("unexpected text after value".into());
    }
    let _ = p.kind;
    if !v.v.is_finite() {
        return Err("not a number".into());
    }
    Ok(match kind {
        Kind::Length if !v.dim => v.v * env.units.scale(),
        _ => v.v,
    })
}

/// Evaluate document variables in order; later ones may reference earlier ones.
pub fn resolve_vars(vars: &[(String, String)], units: Units) -> (HashMap<String, f64>, Vec<Option<String>>) {
    let mut map = HashMap::new();
    let mut errs = vec![];
    for (name, src) in vars {
        let env = Env { vars: &map, units };
        match eval(src, Kind::Length, &env) {
            Ok(v) => {
                map.insert(name.clone(), v);
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
        let env = Env { vars: &vars, units: Units::Mm };
        assert_eq!(eval("width / 2", Kind::Length, &env).unwrap(), 20.0);
        assert_eq!(eval("1 in", Kind::Length, &env).unwrap(), 25.4);
        assert_eq!(eval("2*(3+4)", Kind::Count, &env).unwrap(), 14.0);
        let env = Env { vars: &vars, units: Units::In };
        assert_eq!(eval("2", Kind::Length, &env).unwrap(), 50.8);
        assert_eq!(eval("10 mm", Kind::Length, &env).unwrap(), 10.0);
        assert!(eval("foo", Kind::Length, &env).is_err());
    }
}
