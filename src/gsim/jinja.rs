//! A small Jinja2 interpreter for Klipper's G-code templates: `{ expr }` substitutions,
//! `{% set %}`, `{% if %}/{% elif %}/{% else %}`, `{% for %}`, `{# comments #}` and the
//! whitespace dashes. Expressions cover what printer macros use: numbers, strings, lists,
//! dicts, attribute and item lookup, arithmetic, comparisons, `and`/`or`/`not`, `in`,
//! `a if c else b`, `~`, `%` string formatting, the common filters (`float`, `int`,
//! `default`, `round`, `abs`, `min`, `max`, `lower`, …) and calls (`range`,
//! `action_raise_error`, `action_respond_info`, `dict.get`, `str.lower`, …).

use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    None,
    /// A name or attribute that isn't defined (falsy, 0 in arithmetic, "" as text).
    Undef(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    List(Rc<Vec<V>>),
    Dict(Rc<BTreeMap<String, V>>),
}

impl V {
    pub fn s(x: &str) -> V {
        V::Str(Rc::from(x))
    }
    pub fn dict(m: BTreeMap<String, V>) -> V {
        V::Dict(Rc::new(m))
    }
    pub fn list(v: Vec<V>) -> V {
        V::List(Rc::new(v))
    }
    /// A Klipper coordinate: `.x .y .z .e` and `[0..3]`.
    pub fn coord(c: [f64; 4]) -> V {
        let mut m = BTreeMap::new();
        for (k, n) in ["x", "y", "z", "e"].iter().zip(c) {
            m.insert(k.to_string(), V::Float(n));
        }
        m.insert("__list__".into(), V::list(c.iter().map(|v| V::Float(*v)).collect()));
        V::dict(m)
    }
    pub fn truthy(&self) -> bool {
        match self {
            V::None | V::Undef(_) => false,
            V::Bool(b) => *b,
            V::Int(i) => *i != 0,
            V::Float(f) => *f != 0.0,
            V::Str(s) => !s.is_empty(),
            V::List(l) => !l.is_empty(),
            V::Dict(d) => !d.is_empty(),
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            V::Bool(b) => Some(*b as i64 as f64),
            V::Int(i) => Some(*i as f64),
            V::Float(f) => Some(*f),
            V::Undef(_) | V::None => Some(0.0),
            V::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn is_int(&self) -> bool {
        matches!(self, V::Int(_) | V::Bool(_))
    }
    /// How the value prints into G-code (Python's str()).
    pub fn text(&self) -> String {
        match self {
            V::None => "None".into(),
            V::Undef(_) => String::new(),
            V::Bool(b) => if *b { "True" } else { "False" }.into(),
            V::Int(i) => i.to_string(),
            V::Float(f) => py_float(*f),
            V::Str(s) => s.to_string(),
            V::List(l) => format!("[{}]", l.iter().map(|v| v.repr()).collect::<Vec<_>>().join(", ")),
            V::Dict(d) => {
                if let Some(l) = d.get("__list__") {
                    return l.text();
                }
                format!("{{{}}}", d.iter().map(|(k, v)| format!("'{k}': {}", v.repr())).collect::<Vec<_>>().join(", "))
            }
        }
    }
    fn repr(&self) -> String {
        match self {
            V::Str(s) => format!("'{s}'"),
            v => v.text(),
        }
    }
    fn as_list(&self) -> Option<Vec<V>> {
        match self {
            V::List(l) => Some(l.as_ref().clone()),
            V::Str(s) => Some(s.chars().map(|c| V::s(&c.to_string())).collect()),
            V::Dict(d) => match d.get("__list__") {
                Some(V::List(l)) => Some(l.as_ref().clone()),
                _ => Some(d.keys().map(|k| V::s(k)).collect()),
            },
            V::Undef(_) | V::None => Some(vec![]),
            _ => None,
        }
    }
}

pub fn py_float(f: f64) -> String {
    if f.is_finite() && f == f.trunc() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        let s = format!("{f}");
        s
    }
}

// ---------------------------------------------------------------- expressions

#[derive(Clone, Debug)]
pub enum E {
    Lit(V),
    Name(String),
    List(Vec<E>),
    Dict(Vec<(E, E)>),
    Attr(Box<E>, String),
    Item(Box<E>, Box<E>),
    Call(Box<E>, Vec<E>, Vec<(String, E)>),
    Filter(Box<E>, String, Vec<E>),
    Un(&'static str, Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Cond(Box<E>, Box<E>, Box<E>),
    Test(Box<E>, String, bool),
}

#[derive(Clone, Debug, PartialEq)]
enum T {
    Num(f64, bool),
    Str(String),
    Id(String),
    Op(&'static str),
}

const OPS: [&str; 27] = ["**", "//", "==", "!=", "<=", ">=", "(", ")", "[", "]", "{", "}", ",", ":", ".", "|", "+", "-", "*", "/", "%", "<", ">", "~", "=", "!", "?"];

fn lex(s: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() || (ch == '.' && i + 1 < c.len() && c[i + 1].is_ascii_digit()) {
            let st = i;
            let mut float = false;
            while i < c.len()
                && (c[i].is_ascii_digit()
                    || c[i] == '.'
                    || c[i] == '_'
                    || ((c[i] == 'e' || c[i] == 'E') && i + 1 < c.len() && (c[i + 1].is_ascii_digit() || c[i + 1] == '-' || c[i + 1] == '+')))
            {
                if c[i] == '.' {
                    // `1.x` is an attribute, not a float
                    if i + 1 < c.len() && c[i + 1].is_alphabetic() {
                        break;
                    }
                    float = true;
                }
                if c[i] == 'e' || c[i] == 'E' {
                    float = true;
                    i += 1;
                }
                i += 1;
            }
            let t: String = c[st..i].iter().filter(|c| **c != '_').collect();
            out.push(T::Num(t.parse().map_err(|_| format!("bad number {t}"))?, float));
        } else if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(T::Id(c[st..i].iter().collect()));
        } else if ch == '\'' || ch == '"' {
            let q = ch;
            i += 1;
            let mut t = String::new();
            while i < c.len() && c[i] != q {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                    t.push(match c[i] {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                } else {
                    t.push(c[i]);
                }
                i += 1;
            }
            if i >= c.len() {
                return Err("unterminated string".into());
            }
            i += 1;
            out.push(T::Str(t));
        } else {
            let rest: String = c[i..(i + 2).min(c.len())].iter().collect();
            let op = OPS.iter().find(|o| rest.starts_with(**o)).ok_or(format!("unexpected '{ch}'"))?;
            i += op.chars().count();
            out.push(T::Op(op));
        }
    }
    Ok(out)
}

struct P {
    t: Vec<T>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn op(&mut self, o: &str) -> bool {
        if matches!(self.peek(), Some(T::Op(x)) if *x == o) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn kw(&mut self, k: &str) -> bool {
        if matches!(self.peek(), Some(T::Id(x)) if x == k) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn is_kw(&self, k: &str, off: usize) -> bool {
        matches!(self.t.get(self.i + off), Some(T::Id(x)) if x == k)
    }
    fn expect(&mut self, o: &str) -> Result<(), String> {
        if self.op(o) {
            Ok(())
        } else {
            Err(format!("expected '{o}'"))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        match self.t.get(self.i).cloned() {
            Some(T::Id(s)) => {
                self.i += 1;
                Ok(s)
            }
            _ => Err("expected a name".into()),
        }
    }
    fn expr(&mut self) -> Result<E, String> {
        let a = self.or()?;
        if self.is_kw("if", 0) {
            self.i += 1;
            let c = self.or()?;
            let b = if self.kw("else") { self.expr()? } else { E::Lit(V::Undef(String::new())) };
            return Ok(E::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(a)
    }
    fn or(&mut self) -> Result<E, String> {
        let mut a = self.and()?;
        while self.kw("or") {
            a = E::Bin("or", Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }
    fn and(&mut self) -> Result<E, String> {
        let mut a = self.not()?;
        while self.kw("and") {
            a = E::Bin("and", Box::new(a), Box::new(self.not()?));
        }
        Ok(a)
    }
    fn not(&mut self) -> Result<E, String> {
        if self.kw("not") {
            return Ok(E::Un("not", Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<E, String> {
        let mut a = self.concat()?;
        loop {
            let op = match self.peek() {
                Some(T::Op(o)) if ["==", "!=", "<", ">", "<=", ">="].contains(o) => *o,
                Some(T::Id(k)) if k == "in" => "in",
                Some(T::Id(k)) if k == "not" && self.is_kw("in", 1) => {
                    self.i += 1;
                    "not in"
                }
                Some(T::Id(k)) if k == "is" => {
                    self.i += 1;
                    let neg = self.kw("not");
                    let name = self.ident()?;
                    a = E::Test(Box::new(a), name, neg);
                    continue;
                }
                _ => break,
            };
            self.i += 1;
            a = E::Bin(op, Box::new(a), Box::new(self.concat()?));
        }
        Ok(a)
    }
    fn concat(&mut self) -> Result<E, String> {
        let mut a = self.add()?;
        while self.op("~") {
            a = E::Bin("~", Box::new(a), Box::new(self.add()?));
        }
        Ok(a)
    }
    fn add(&mut self) -> Result<E, String> {
        let mut a = self.mul()?;
        loop {
            let op = if self.op("+") {
                "+"
            } else if self.op("-") {
                "-"
            } else {
                break;
            };
            a = E::Bin(op, Box::new(a), Box::new(self.mul()?));
        }
        Ok(a)
    }
    fn mul(&mut self) -> Result<E, String> {
        let mut a = self.unary()?;
        loop {
            let op = ["//", "*", "/", "%"].into_iter().find(|o| matches!(self.peek(), Some(T::Op(x)) if x == o));
            let Some(op) = op else { break };
            self.i += 1;
            a = E::Bin(op, Box::new(a), Box::new(self.unary()?));
        }
        Ok(a)
    }
    fn unary(&mut self) -> Result<E, String> {
        if self.op("-") {
            return Ok(E::Un("-", Box::new(self.unary()?)));
        }
        if self.op("+") {
            return self.unary();
        }
        self.pow()
    }
    fn pow(&mut self) -> Result<E, String> {
        let a = self.postfix()?;
        if self.op("**") {
            return Ok(E::Bin("**", Box::new(a), Box::new(self.unary()?)));
        }
        Ok(a)
    }
    fn args(&mut self, close: &str) -> Result<(Vec<E>, Vec<(String, E)>), String> {
        let (mut pos, mut kw) = (vec![], vec![]);
        while !self.op(close) {
            if let (Some(T::Id(k)), Some(T::Op("="))) = (self.t.get(self.i).cloned(), self.t.get(self.i + 1)) {
                self.i += 2;
                kw.push((k, self.expr()?));
            } else {
                pos.push(self.expr()?);
            }
            if !self.op(",") {
                self.expect(close)?;
                break;
            }
        }
        Ok((pos, kw))
    }
    fn postfix(&mut self) -> Result<E, String> {
        let mut a = self.primary()?;
        loop {
            if self.op(".") {
                let n = match self.t.get(self.i).cloned() {
                    Some(T::Id(s)) => s,
                    Some(T::Num(n, _)) => (n as i64).to_string(),
                    _ => return Err("expected an attribute name".into()),
                };
                self.i += 1;
                a = E::Attr(Box::new(a), n);
            } else if self.op("[") {
                let k = self.expr()?;
                self.expect("]")?;
                a = E::Item(Box::new(a), Box::new(k));
            } else if self.op("(") {
                let (p, k) = self.args(")")?;
                a = E::Call(Box::new(a), p, k);
            } else if self.op("|") {
                let name = self.ident()?;
                let args = if self.op("(") { self.args(")")?.0 } else { vec![] };
                a = E::Filter(Box::new(a), name, args);
            } else {
                break;
            }
        }
        Ok(a)
    }
    fn primary(&mut self) -> Result<E, String> {
        let t = self.t.get(self.i).cloned().ok_or("expression ends early")?;
        self.i += 1;
        Ok(match t {
            T::Num(n, f) => E::Lit(if f { V::Float(n) } else { V::Int(n as i64) }),
            T::Str(s) => {
                // adjacent string literals join
                let mut s = s;
                while let Some(T::Str(more)) = self.peek().cloned() {
                    self.i += 1;
                    s.push_str(&more);
                }
                E::Lit(V::s(&s))
            }
            T::Id(k) => match k.as_str() {
                "True" | "true" => E::Lit(V::Bool(true)),
                "False" | "false" => E::Lit(V::Bool(false)),
                "None" | "none" => E::Lit(V::None),
                _ => E::Name(k),
            },
            T::Op("(") => {
                if self.op(")") {
                    return Ok(E::List(vec![]));
                }
                let e = self.expr()?;
                if self.op(",") {
                    let mut items = vec![e];
                    while !self.op(")") {
                        items.push(self.expr()?);
                        if !self.op(",") {
                            self.expect(")")?;
                            break;
                        }
                    }
                    return Ok(E::List(items));
                }
                self.expect(")")?;
                e
            }
            T::Op("[") => {
                let mut items = vec![];
                while !self.op("]") {
                    items.push(self.expr()?);
                    if !self.op(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                E::List(items)
            }
            T::Op("{") => {
                let mut items = vec![];
                while !self.op("}") {
                    let k = self.expr()?;
                    self.expect(":")?;
                    items.push((k, self.expr()?));
                    if !self.op(",") {
                        self.expect("}")?;
                        break;
                    }
                }
                E::Dict(items)
            }
            T::Op(o) => return Err(format!("unexpected '{o}'")),
        })
    }
}

pub fn parse_expr(s: &str) -> Result<E, String> {
    let mut p = P { t: lex(s)?, i: 0 };
    let e = p.expr()?;
    if p.i < p.t.len() {
        return Err(format!("unexpected {:?} in `{s}`", p.t[p.i]));
    }
    Ok(e)
}

/// A Python literal as Klipper reads `variable_*` values and SET_GCODE_VARIABLE VALUE= (falls back to the text).
pub fn literal(s: &str) -> V {
    let s = s.trim();
    match parse_expr(s) {
        Ok(e) => {
            let mut ctx = Ctx::default();
            match ctx.eval(&e) {
                Ok(V::Undef(_)) | Err(_) => V::s(s),
                Ok(v) => v,
            }
        }
        Err(_) => V::s(s),
    }
}

// ---------------------------------------------------------------- evaluation

/// What a template sees: the names and what came of rendering it.
#[derive(Default)]
pub struct Ctx {
    pub scopes: Vec<BTreeMap<String, V>>,
    /// action_respond_info messages
    pub info: Vec<String>,
    /// action_raise_error: the macro stops
    pub error: Option<String>,
    /// names/attributes looked up that don't exist
    pub undefined: Vec<String>,
}

fn num_op(op: &str, a: &V, b: &V) -> Result<V, String> {
    let (x, y) = (a.num().ok_or(format!("{} is not a number", a.repr()))?, b.num().ok_or(format!("{} is not a number", b.repr()))?);
    let ints = a.is_int() && b.is_int();
    Ok(match op {
        "+" if ints => V::Int(x as i64 + y as i64),
        "-" if ints => V::Int(x as i64 - y as i64),
        "*" if ints => V::Int(x as i64 * y as i64),
        "+" => V::Float(x + y),
        "-" => V::Float(x - y),
        "*" => V::Float(x * y),
        "/" => {
            if y == 0.0 {
                return Err("division by zero".into());
            }
            V::Float(x / y)
        }
        "//" => {
            if y == 0.0 {
                return Err("division by zero".into());
            }
            if ints {
                V::Int((x / y).floor() as i64)
            } else {
                V::Float((x / y).floor())
            }
        }
        "%" => {
            if y == 0.0 {
                return Err("modulo by zero".into());
            }
            let r = x - y * (x / y).floor();
            if ints {
                V::Int(r as i64)
            } else {
                V::Float(r)
            }
        }
        "**" => {
            if ints && y >= 0.0 {
                V::Int(x.powf(y) as i64)
            } else {
                V::Float(x.powf(y))
            }
        }
        _ => unreachable!(),
    })
}

/// Python's `fmt % args` for %d %i %f %s %r %x %e %g with flags/width/precision.
pub fn percent_format(fmt: &str, args: &[V]) -> Result<String, String> {
    let c: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut ai = 0;
    let mut i = 0;
    while i < c.len() {
        if c[i] != '%' {
            out.push(c[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < c.len() && c[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        let mut flags = String::new();
        while i < c.len() && "-+ 0#".contains(c[i]) {
            flags.push(c[i]);
            i += 1;
        }
        let mut width = String::new();
        while i < c.len() && c[i].is_ascii_digit() {
            width.push(c[i]);
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < c.len() && c[i] == '.' {
            i += 1;
            let mut p = String::new();
            while i < c.len() && c[i].is_ascii_digit() {
                p.push(c[i]);
                i += 1;
            }
            prec = Some(p.parse().unwrap_or(0));
        }
        let conv = *c.get(i).ok_or("format ends early")?;
        i += 1;
        let v = args.get(ai).cloned().ok_or("not enough arguments for format string")?;
        ai += 1;
        let body = match conv {
            'd' | 'i' => format!("{}", v.num().ok_or("%d needs a number")?.trunc() as i64),
            'f' | 'F' => format!("{:.*}", prec.unwrap_or(6), v.num().ok_or("%f needs a number")?),
            'e' | 'E' => format!("{:.*e}", prec.unwrap_or(6), v.num().ok_or("%e needs a number")?),
            'g' | 'G' => {
                let n = v.num().ok_or("%g needs a number")?;
                let s = format!("{:.*}", prec.unwrap_or(6), n);
                if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    s
                }
            }
            'x' => format!("{:x}", v.num().unwrap_or(0.0) as i64),
            's' => v.text(),
            'r' => v.repr(),
            o => return Err(format!("unsupported format %{o}")),
        };
        let body = if flags.contains('+') && !body.starts_with('-') && "dfieg".contains(conv) { format!("+{body}") } else { body };
        let w: usize = width.parse().unwrap_or(0);
        if body.chars().count() < w {
            let pad = w - body.chars().count();
            if flags.contains('-') {
                out.push_str(&body);
                out.push_str(&" ".repeat(pad));
            } else if flags.contains('0') && conv != 's' {
                let (sign, digits) = if body.starts_with('-') || body.starts_with('+') { body.split_at(1) } else { ("", body.as_str()) };
                out.push_str(sign);
                out.push_str(&"0".repeat(pad));
                out.push_str(digits);
            } else {
                out.push_str(&" ".repeat(pad));
                out.push_str(&body);
            }
        } else {
            out.push_str(&body);
        }
    }
    Ok(out)
}

fn cmp_vals(a: &V, b: &V) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (V::Str(x), V::Str(y)) => Some(x.cmp(y)),
        _ => a.num()?.partial_cmp(&b.num()?),
    }
}

fn eq_vals(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Str(x), V::Str(y)) => x == y,
        (V::Str(_), _) | (_, V::Str(_)) => false,
        (V::None, V::None) => true,
        (V::None, _) | (_, V::None) => false,
        (V::List(x), V::List(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| eq_vals(p, q)),
        _ => match (a.num(), b.num()) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

fn contains(hay: &V, needle: &V) -> bool {
    match hay {
        V::Str(s) => s.contains(needle.text().as_str()),
        V::Dict(d) if !d.contains_key("__list__") => d.contains_key(&needle.text()),
        h => h.as_list().map(|l| l.iter().any(|x| eq_vals(x, needle))).unwrap_or(false),
    }
}

impl Ctx {
    pub fn with(vars: BTreeMap<String, V>) -> Ctx {
        Ctx { scopes: vec![vars], ..Default::default() }
    }
    fn get(&self, n: &str) -> Option<V> {
        self.scopes.iter().rev().find_map(|s| s.get(n).cloned())
    }
    pub fn set(&mut self, n: &str, v: V) {
        if self.scopes.is_empty() {
            self.scopes.push(BTreeMap::new());
        }
        self.scopes.last_mut().unwrap().insert(n.to_string(), v);
    }

    fn attr(&mut self, o: &V, k: &str) -> V {
        match o {
            V::Dict(d) => {
                if let Some(v) = d.get(k) {
                    return v.clone();
                }
                // printer["gcode_macro X"] keys are case-insensitive in Klipper's lookups of params
                if let Some((_, v)) = d.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)) {
                    return v.clone();
                }
                self.undefined.push(k.to_string());
                V::Undef(k.to_string())
            }
            V::Undef(n) => {
                let p = format!("{n}.{k}");
                V::Undef(p)
            }
            _ => {
                self.undefined.push(k.to_string());
                V::Undef(k.to_string())
            }
        }
    }

    fn item(&mut self, o: &V, k: &V) -> Result<V, String> {
        match (o, k) {
            (V::Dict(d), V::Int(i)) if d.contains_key("__list__") => self.item(&d["__list__"], &V::Int(*i)),
            (V::Dict(_), k) => Ok(self.attr(o, &k.text())),
            (V::List(l), k) => {
                let i = k.num().ok_or("list index must be a number")? as i64;
                let n = l.len() as i64;
                let j = if i < 0 { n + i } else { i };
                l.get(j as usize).cloned().ok_or(format!("list index {i} out of range"))
            }
            (V::Str(s), k) => {
                let c: Vec<char> = s.chars().collect();
                let i = k.num().ok_or("string index must be a number")? as i64;
                let j = if i < 0 { c.len() as i64 + i } else { i };
                c.get(j as usize).map(|c| V::s(&c.to_string())).ok_or("string index out of range".into())
            }
            (V::Undef(n), k) => Ok(V::Undef(format!("{n}[{}]", k.text()))),
            _ => Err(format!("can't index {}", o.repr())),
        }
    }

    pub fn eval(&mut self, e: &E) -> Result<V, String> {
        Ok(match e {
            E::Lit(v) => v.clone(),
            E::Name(n) => match self.get(n) {
                Some(v) => v,
                None => {
                    self.undefined.push(n.clone());
                    V::Undef(n.clone())
                }
            },
            E::List(items) => V::list(items.iter().map(|x| self.eval(x)).collect::<Result<_, _>>()?),
            E::Dict(items) => {
                let mut m = BTreeMap::new();
                for (k, v) in items {
                    let k = self.eval(k)?.text();
                    m.insert(k, self.eval(v)?);
                }
                V::dict(m)
            }
            E::Attr(o, k) => {
                let o = self.eval(o)?;
                self.attr(&o, k)
            }
            E::Item(o, k) => {
                let o = self.eval(o)?;
                let k = self.eval(k)?;
                self.item(&o, &k)?
            }
            E::Un("not", a) => V::Bool(!self.eval(a)?.truthy()),
            E::Un(_, a) => {
                let v = self.eval(a)?;
                match v {
                    V::Int(i) => V::Int(-i),
                    v => V::Float(-v.num().ok_or("can't negate that")?),
                }
            }
            E::Bin("and", a, b) => {
                let x = self.eval(a)?;
                if !x.truthy() {
                    x
                } else {
                    self.eval(b)?
                }
            }
            E::Bin("or", a, b) => {
                let x = self.eval(a)?;
                if x.truthy() {
                    x
                } else {
                    self.eval(b)?
                }
            }
            E::Bin(op, a, b) => {
                let (x, y) = (self.eval(a)?, self.eval(b)?);
                match *op {
                    "~" => V::s(&format!("{}{}", x.text(), y.text())),
                    "+" => match (&x, &y) {
                        (V::Str(p), V::Str(q)) => V::s(&format!("{p}{q}")),
                        (V::List(p), V::List(q)) => V::list(p.iter().chain(q.iter()).cloned().collect()),
                        _ => num_op("+", &x, &y)?,
                    },
                    "*" => match (&x, &y) {
                        (V::Str(p), n) | (n, V::Str(p)) if n.is_int() => V::s(&p.repeat(n.num().unwrap_or(0.0).max(0.0) as usize)),
                        _ => num_op("*", &x, &y)?,
                    },
                    "%" => match &x {
                        V::Str(f) => {
                            let args = match &y {
                                V::List(l) => l.as_ref().clone(),
                                v => vec![v.clone()],
                            };
                            V::s(&percent_format(f, &args)?)
                        }
                        _ => num_op("%", &x, &y)?,
                    },
                    "==" => V::Bool(eq_vals(&x, &y)),
                    "!=" => V::Bool(!eq_vals(&x, &y)),
                    "<" => V::Bool(cmp_vals(&x, &y) == Some(std::cmp::Ordering::Less)),
                    ">" => V::Bool(cmp_vals(&x, &y) == Some(std::cmp::Ordering::Greater)),
                    "<=" => V::Bool(matches!(cmp_vals(&x, &y), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal))),
                    ">=" => V::Bool(matches!(cmp_vals(&x, &y), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal))),
                    "in" => V::Bool(contains(&y, &x)),
                    "not in" => V::Bool(!contains(&y, &x)),
                    o => num_op(o, &x, &y)?,
                }
            }
            E::Cond(c, a, b) => {
                if self.eval(c)?.truthy() {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            E::Test(a, name, neg) => {
                let v = self.eval(a)?;
                let r = match name.as_str() {
                    "defined" => !matches!(v, V::Undef(_)),
                    "undefined" => matches!(v, V::Undef(_)),
                    "none" => matches!(v, V::None),
                    "number" => matches!(v, V::Int(_) | V::Float(_)),
                    "string" => matches!(v, V::Str(_)),
                    "even" => v.num().map(|n| n as i64 % 2 == 0).unwrap_or(false),
                    "odd" => v.num().map(|n| n as i64 % 2 != 0).unwrap_or(false),
                    t => return Err(format!("unknown test '{t}'")),
                };
                V::Bool(r != *neg)
            }
            E::Filter(a, name, args) => {
                let v = self.eval(a)?;
                let args: Vec<V> = args.iter().map(|x| self.eval(x)).collect::<Result<_, _>>()?;
                self.filter(v, name, &args)?
            }
            E::Call(f, args, kw) => {
                let args: Vec<V> = args.iter().map(|x| self.eval(x)).collect::<Result<_, _>>()?;
                let kw: Vec<(String, V)> = kw.iter().map(|(k, x)| Ok((k.clone(), self.eval(x)?))).collect::<Result<_, String>>()?;
                match f.as_ref() {
                    E::Name(n) => self.call(n, &args, &kw)?,
                    E::Attr(o, m) => {
                        let o = self.eval(o)?;
                        self.method(&o, m, &args)?
                    }
                    _ => return Err("can't call that".into()),
                }
            }
        })
    }

    fn call(&mut self, n: &str, a: &[V], kw: &[(String, V)]) -> Result<V, String> {
        let num = |i: usize| a.get(i).and_then(|v| v.num()).unwrap_or(0.0) as i64;
        Ok(match n {
            "range" => {
                let (s, e, st) = match a.len() {
                    1 => (0, num(0), 1),
                    2 => (num(0), num(1), 1),
                    _ => (num(0), num(1), num(2)),
                };
                if st == 0 {
                    return Err("range() step is 0".into());
                }
                let mut v = vec![];
                let mut i = s;
                while (st > 0 && i < e) || (st < 0 && i > e) {
                    v.push(V::Int(i));
                    i += st;
                    if v.len() > 1_000_000 {
                        return Err("range() too long".into());
                    }
                }
                V::list(v)
            }
            "action_respond_info" => {
                self.info.push(a.first().map(|v| v.text()).unwrap_or_default());
                V::s("")
            }
            "action_raise_error" => {
                let m = a.first().map(|v| v.text()).unwrap_or_default();
                if self.error.is_none() {
                    self.error = Some(m);
                }
                V::s("")
            }
            "action_emergency_stop" => {
                self.error.get_or_insert_with(|| format!("emergency stop: {}", a.first().map(|v| v.text()).unwrap_or_default()));
                V::s("")
            }
            "action_call_remote_method" => V::s(""),
            "dict" => V::dict(kw.iter().cloned().collect()),
            "float" => V::Float(a.first().and_then(|v| v.num()).unwrap_or(0.0)),
            "int" => V::Int(a.first().and_then(|v| v.num()).unwrap_or(0.0) as i64),
            "str" => V::s(&a.first().map(|v| v.text()).unwrap_or_default()),
            "abs" | "min" | "max" | "round" | "len" | "sum" | "list" | "bool" => {
                let v = a.first().cloned().unwrap_or(V::None);
                let v = if matches!(n, "min" | "max") && a.len() > 1 { V::list(a.to_vec()) } else { v };
                self.filter(v, if n == "len" { "length" } else { n }, &a[a.len().min(1)..])?
            }
            _ => {
                self.undefined.push(format!("{n}()"));
                V::Undef(format!("{n}()"))
            }
        })
    }

    fn method(&mut self, o: &V, m: &str, a: &[V]) -> Result<V, String> {
        Ok(match (o, m) {
            (V::Dict(d), "get") => {
                let k = a.first().map(|v| v.text()).unwrap_or_default();
                d.get(&k).cloned().unwrap_or_else(|| a.get(1).cloned().unwrap_or(V::None))
            }
            (V::Dict(d), "keys") => V::list(d.keys().filter(|k| *k != "__list__").map(|k| V::s(k)).collect()),
            (V::Dict(d), "values") => V::list(d.iter().filter(|(k, _)| *k != "__list__").map(|(_, v)| v.clone()).collect()),
            (V::Dict(d), "items") => V::list(d.iter().filter(|(k, _)| *k != "__list__").map(|(k, v)| V::list(vec![V::s(k), v.clone()])).collect()),
            (V::Str(s), "lower") => V::s(&s.to_lowercase()),
            (V::Str(s), "upper") => V::s(&s.to_uppercase()),
            (V::Str(s), "strip") => V::s(s.trim()),
            (V::Str(s), "startswith") => V::Bool(s.starts_with(a.first().map(|v| v.text()).unwrap_or_default().as_str())),
            (V::Str(s), "endswith") => V::Bool(s.ends_with(a.first().map(|v| v.text()).unwrap_or_default().as_str())),
            (V::Str(s), "replace") => V::s(&s.replace(&a.first().map(|v| v.text()).unwrap_or_default(), &a.get(1).map(|v| v.text()).unwrap_or_default())),
            (V::Str(s), "split") => match a.first() {
                Some(sep) => V::list(s.split(sep.text().as_str()).map(V::s).collect()),
                None => V::list(s.split_whitespace().map(V::s).collect()),
            },
            (V::Str(s), "join") => V::s(&a.first().and_then(|v| v.as_list()).unwrap_or_default().iter().map(|v| v.text()).collect::<Vec<_>>().join(s)),
            (V::Str(s), "format") => {
                let mut out = s.to_string();
                for v in a {
                    out = out.replacen("{}", &v.text(), 1);
                }
                V::s(&out)
            }
            (V::List(_), "append") => {
                // lists are values here; Klipper macros that collect into lists aren't simulated exactly
                self.undefined.push("list.append".into());
                V::None
            }
            (V::List(l), "index") => {
                let x = a.first().cloned().unwrap_or(V::None);
                V::Int(l.iter().position(|v| eq_vals(v, &x)).map(|p| p as i64).unwrap_or(-1))
            }
            (V::List(l), "count") => {
                let x = a.first().cloned().unwrap_or(V::None);
                V::Int(l.iter().filter(|v| eq_vals(v, &x)).count() as i64)
            }
            (V::Undef(n), _) => V::Undef(format!("{n}.{m}()")),
            _ => {
                self.undefined.push(format!(".{m}()"));
                V::Undef(format!(".{m}()"))
            }
        })
    }

    fn filter(&mut self, v: V, name: &str, a: &[V]) -> Result<V, String> {
        Ok(match name {
            "float" => V::Float(v.num().unwrap_or_else(|| a.first().and_then(|d| d.num()).unwrap_or(0.0))),
            "int" => V::Int(match &v {
                V::Str(s) => s.trim().parse::<f64>().map(|f| f.trunc()).unwrap_or_else(|_| a.first().and_then(|d| d.num()).unwrap_or(0.0)) as i64,
                v => v.num().unwrap_or(0.0).trunc() as i64,
            }),
            "default" | "d" => {
                let boolean = a.get(1).map(|b| b.truthy()).unwrap_or(false);
                if matches!(v, V::Undef(_)) || (boolean && !v.truthy()) {
                    a.first().cloned().unwrap_or(V::s(""))
                } else {
                    v
                }
            }
            "round" => {
                let p = a.first().and_then(|x| x.num()).unwrap_or(0.0) as i32;
                let method = a.get(1).map(|m| m.text()).unwrap_or_else(|| "common".into());
                let k = 10f64.powi(p);
                let x = v.num().unwrap_or(0.0) * k;
                let r = match method.as_str() {
                    "ceil" => x.ceil(),
                    "floor" => x.floor(),
                    _ => x.round(),
                };
                V::Float(r / k)
            }
            "abs" => match v {
                V::Int(i) => V::Int(i.abs()),
                v => V::Float(v.num().unwrap_or(0.0).abs()),
            },
            "string" | "str" => V::s(&v.text()),
            "lower" => V::s(&v.text().to_lowercase()),
            "upper" => V::s(&v.text().to_uppercase()),
            "trim" => V::s(v.text().trim()),
            "capitalize" => {
                let t = v.text();
                let mut c = t.chars();
                V::s(&c.next().map(|f| format!("{}{}", f.to_uppercase(), c.as_str().to_lowercase())).unwrap_or_default())
            }
            "bool" => V::Bool(match &v {
                V::Str(s) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes" | "on"),
                v => v.truthy(),
            }),
            "length" | "count" => V::Int(v.as_list().map(|l| l.len()).unwrap_or(0) as i64),
            "list" => V::list(v.as_list().unwrap_or_default()),
            "first" => v.as_list().and_then(|l| l.first().cloned()).unwrap_or(V::Undef("first".into())),
            "last" => v.as_list().and_then(|l| l.last().cloned()).unwrap_or(V::Undef("last".into())),
            "join" => V::s(&v.as_list().unwrap_or_default().iter().map(|x| x.text()).collect::<Vec<_>>().join(&a.first().map(|s| s.text()).unwrap_or_default())),
            "replace" => V::s(&v.text().replace(&a.first().map(|s| s.text()).unwrap_or_default(), &a.get(1).map(|s| s.text()).unwrap_or_default())),
            "sum" => {
                let l = v.as_list().unwrap_or_default();
                if l.iter().all(|x| x.is_int()) {
                    V::Int(l.iter().map(|x| x.num().unwrap_or(0.0) as i64).sum())
                } else {
                    V::Float(l.iter().filter_map(|x| x.num()).sum())
                }
            }
            "min" | "max" => {
                let l = v.as_list().unwrap_or_default();
                let mut best: Option<V> = None;
                for x in l {
                    let better = match &best {
                        None => true,
                        Some(b) => {
                            let o = cmp_vals(&x, b);
                            if name == "min" {
                                o == Some(std::cmp::Ordering::Less)
                            } else {
                                o == Some(std::cmp::Ordering::Greater)
                            }
                        }
                    };
                    if better {
                        best = Some(x);
                    }
                }
                best.unwrap_or(V::Undef(name.into()))
            }
            "sort" => {
                let mut l = v.as_list().unwrap_or_default();
                l.sort_by(|x, y| cmp_vals(x, y).unwrap_or(std::cmp::Ordering::Equal));
                if a.first().map(|r| r.truthy()).unwrap_or(false) {
                    l.reverse();
                }
                V::list(l)
            }
            "reverse" => {
                let mut l = v.as_list().unwrap_or_default();
                l.reverse();
                V::list(l)
            }
            "unique" => {
                let mut out: Vec<V> = vec![];
                for x in v.as_list().unwrap_or_default() {
                    if !out.iter().any(|y| eq_vals(y, &x)) {
                        out.push(x);
                    }
                }
                V::list(out)
            }
            "tojson" => V::s(&v.repr()),
            "format" => match &v {
                V::Str(f) => V::s(&percent_format(f, a)?),
                _ => v,
            },
            f => return Err(format!("unknown filter '{f}'")),
        })
    }
}

// ---------------------------------------------------------------- templates

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Out(E),
    Set(Vec<String>, E),
    If(Vec<(E, Vec<Node>)>, Vec<Node>),
    For(Vec<String>, E, Vec<Node>, Vec<Node>),
}

#[derive(Clone, Debug)]
enum Tok {
    Text(String),
    Out(String),
    Block(String),
}

/// Split a template into text, `{ expr }` and `{% block %}` pieces (`{{ }}` works too).
fn tokens(src: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = src.chars().collect();
    let mut out: Vec<Tok> = vec![];
    let mut text = String::new();
    let mut i = 0;
    let mut strip_next = false;
    let push_text = |out: &mut Vec<Tok>, text: &mut String, strip_left: bool| {
        if strip_left {
            let t = text.trim_end().to_string();
            *text = t;
        }
        if !text.is_empty() {
            out.push(Tok::Text(std::mem::take(text)));
        }
    };
    while i < c.len() {
        if strip_next {
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            strip_next = false;
            continue;
        }
        if c[i] == '{' && i + 1 < c.len() && (c[i + 1] == '%' || c[i + 1] == '#') {
            let kind = c[i + 1];
            let close = if kind == '%' { "%}" } else { "#}" };
            let rest: String = c[i + 2..].iter().collect();
            let end = rest.find(close).ok_or(format!("unclosed {{{kind}"))?;
            let mut body: String = rest[..end].to_string();
            let strip_left = body.starts_with('-');
            if strip_left {
                body.remove(0);
            }
            if body.ends_with('-') {
                body.pop();
                strip_next = true;
            }
            push_text(&mut out, &mut text, strip_left);
            if kind == '%' {
                out.push(Tok::Block(body.trim().to_string()));
            }
            i += 2 + rest[..end].chars().count() + 2;
            continue;
        }
        if c[i] == '{' {
            // find the matching '}' (strings and nested braces)
            let double = i + 1 < c.len() && c[i + 1] == '{';
            let mut j = i + if double { 2 } else { 1 };
            let mut depth = 0;
            let mut q: Option<char> = None;
            let st = j;
            while j < c.len() {
                let ch = c[j];
                if let Some(qc) = q {
                    if ch == '\\' {
                        j += 1;
                    } else if ch == qc {
                        q = None;
                    }
                } else if ch == '\'' || ch == '"' {
                    q = Some(ch);
                } else if ch == '{' {
                    depth += 1;
                } else if ch == '}' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                j += 1;
            }
            if j >= c.len() {
                return Err("unclosed {".into());
            }
            let body: String = c[st..j].iter().collect();
            j += 1;
            if double && j < c.len() && c[j] == '}' {
                j += 1;
            }
            push_text(&mut out, &mut text, false);
            out.push(Tok::Out(body));
            i = j;
            continue;
        }
        text.push(c[i]);
        i += 1;
    }
    push_text(&mut out, &mut text, false);
    Ok(out)
}

fn kw_split(b: &str) -> (&str, &str) {
    let b = b.trim();
    match b.find(|c: char| c.is_whitespace()) {
        Some(p) => (&b[..p], b[p..].trim()),
        None => (b, ""),
    }
}

fn parse_nodes(toks: &[Tok], i: &mut usize, stop: &[&str]) -> Result<(Vec<Node>, String), String> {
    let mut out = vec![];
    while *i < toks.len() {
        match &toks[*i] {
            Tok::Text(t) => {
                out.push(Node::Text(t.clone()));
                *i += 1;
            }
            Tok::Out(e) => {
                out.push(Node::Out(parse_expr(e).map_err(|m| format!("{{{e}}}: {m}"))?));
                *i += 1;
            }
            Tok::Block(b) => {
                let (k, rest) = kw_split(b);
                if stop.contains(&k) {
                    return Ok((out, b.clone()));
                }
                *i += 1;
                match k {
                    "set" => {
                        let eq = rest.find('=').ok_or(format!("{{% {b} %}}: set needs '='"))?;
                        let names: Vec<String> = rest[..eq].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        out.push(Node::Set(names, parse_expr(&rest[eq + 1..]).map_err(|m| format!("{{% {b} %}}: {m}"))?));
                    }
                    "if" => {
                        let mut arms = vec![];
                        let mut cond = parse_expr(rest).map_err(|m| format!("{{% {b} %}}: {m}"))?;
                        let mut els = vec![];
                        loop {
                            let (body, end) = parse_nodes(toks, i, &["elif", "else", "endif"])?;
                            *i += 1;
                            let (ek, er) = kw_split(&end);
                            arms.push((cond.clone(), body));
                            match ek {
                                "elif" => cond = parse_expr(er)?,
                                "else" => {
                                    let (body, _) = parse_nodes(toks, i, &["endif"])?;
                                    *i += 1;
                                    els = body;
                                    break;
                                }
                                _ => break,
                            }
                        }
                        out.push(Node::If(arms, els));
                    }
                    "for" => {
                        let pos = rest.find(" in ").ok_or(format!("{{% {b} %}}: for needs 'in'"))?;
                        let names: Vec<String> = rest[..pos].split(',').map(|s| s.trim().to_string()).collect();
                        let it = parse_expr(&rest[pos + 4..])?;
                        let (body, end) = parse_nodes(toks, i, &["else", "endfor"])?;
                        *i += 1;
                        let mut els = vec![];
                        if kw_split(&end).0 == "else" {
                            els = parse_nodes(toks, i, &["endfor"])?.0;
                            *i += 1;
                        }
                        out.push(Node::For(names, it, body, els));
                    }
                    "raw" | "endraw" => {}
                    other => return Err(format!("unsupported block {{% {other} %}}")),
                }
            }
        }
    }
    if stop.is_empty() {
        Ok((out, String::new()))
    } else {
        Err(format!("missing {{% {} %}}", stop.last().unwrap()))
    }
}

#[derive(Clone, Debug)]
pub struct Template {
    nodes: Vec<Node>,
}

impl Template {
    pub fn parse(src: &str) -> Result<Template, String> {
        let toks = tokens(src)?;
        let mut i = 0;
        let (nodes, _) = parse_nodes(&toks, &mut i, &[])?;
        Ok(Template { nodes })
    }

    pub fn render(&self, ctx: &mut Ctx) -> Result<String, String> {
        let mut out = String::new();
        render_nodes(&self.nodes, ctx, &mut out)?;
        Ok(out)
    }
}

fn render_nodes(nodes: &[Node], ctx: &mut Ctx, out: &mut String) -> Result<(), String> {
    for n in nodes {
        if ctx.error.is_some() {
            return Ok(());
        }
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Out(e) => out.push_str(&ctx.eval(e)?.text()),
            Node::Set(names, e) => {
                let v = ctx.eval(e)?;
                if names.len() == 1 {
                    ctx.set(&names[0], v);
                } else {
                    let l = v.as_list().unwrap_or_default();
                    for (k, n) in names.iter().enumerate() {
                        ctx.set(n, l.get(k).cloned().unwrap_or(V::None));
                    }
                }
            }
            Node::If(arms, els) => {
                let mut done = false;
                for (c, body) in arms {
                    if ctx.eval(c)?.truthy() {
                        render_nodes(body, ctx, out)?;
                        done = true;
                        break;
                    }
                }
                if !done {
                    render_nodes(els, ctx, out)?;
                }
            }
            Node::For(names, it, body, els) => {
                let items = ctx.eval(it)?.as_list().ok_or("for: not a list")?;
                if items.is_empty() {
                    render_nodes(els, ctx, out)?;
                }
                let n = items.len();
                for (k, item) in items.into_iter().enumerate() {
                    // Jinja's loop scope: sets inside the loop don't leak out
                    let mut scope = BTreeMap::new();
                    let mut lp = BTreeMap::new();
                    lp.insert("index".into(), V::Int(k as i64 + 1));
                    lp.insert("index0".into(), V::Int(k as i64));
                    lp.insert("first".into(), V::Bool(k == 0));
                    lp.insert("last".into(), V::Bool(k + 1 == n));
                    lp.insert("length".into(), V::Int(n as i64));
                    scope.insert("loop".into(), V::dict(lp));
                    if names.len() == 1 {
                        scope.insert(names[0].clone(), item);
                    } else {
                        let l = item.as_list().unwrap_or_default();
                        for (j, nm) in names.iter().enumerate() {
                            scope.insert(nm.clone(), l.get(j).cloned().unwrap_or(V::None));
                        }
                    }
                    ctx.scopes.push(scope);
                    let r = render_nodes(body, ctx, out);
                    ctx.scopes.pop();
                    r?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, vars: &[(&str, V)]) -> String {
        let t = Template::parse(src).unwrap();
        let mut c = Ctx::with(vars.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
        t.render(&mut c).unwrap()
    }

    #[test]
    fn klipper_style() {
        let mut p = BTreeMap::new();
        p.insert("TEMP".into(), V::s("215"));
        let params = V::dict(p);
        let src = "{% set temp = params.TEMP|default(0)|float %}\n{% set tool = params.TOOL|default(0)|int %}\nT{tool}{' MIN_TEMP=%d' % temp if temp > 0 else ''}\nM104 S{temp}";
        assert_eq!(run(src, &[("params", params)]).trim(), "T0 MIN_TEMP=215\nM104 S215.0");
    }

    #[test]
    fn loops_and_lists() {
        let src = "{% for i in range(cut|int) %}G1 Y{i}\n{% endfor %}{% set z = [3 + 0.4, 2.0]|min %}Z{z}";
        assert_eq!(run(src, &[("cut", V::Int(2))]), "G1 Y0\nG1 Y1\nZ2.0");
        assert_eq!(run("{% if not 'xy' in homed %}G28 X Y{% endif %}", &[("homed", V::s("xyz"))]), "");
        assert_eq!(run("{% if 'xy' not in homed %}G28{% else %}ok{% endif %}", &[("homed", V::s(""))]), "G28");
    }

    #[test]
    fn lookups() {
        let mut m = BTreeMap::new();
        let mut tr = BTreeMap::new();
        tr.insert("z_lift".into(), V::Float(0.4));
        m.insert("gcode_macro TR_Variables".into(), V::dict(tr));
        m.insert("pos".into(), V::coord([1.0, 2.0, 3.0, 0.0]));
        let printer = V::dict(m);
        assert_eq!(run("{printer[\"gcode_macro TR_Variables\"].z_lift * 2} {printer.pos.y} {printer.pos[2]}", &[("printer", printer)]), "0.8 2.0 3.0");
        assert_eq!(literal("True"), V::Bool(true));
        assert_eq!(literal("[1, 2]"), V::list(vec![V::Int(1), V::Int(2)]));
        assert_eq!(literal("hello there"), V::s("hello there"));
        assert_eq!(percent_format("%.2f|%5d|%s", &[V::Float(1.234), V::Int(42), V::s("x")]).unwrap(), "1.23|   42|x");
    }
}
