//! Arbitrary source waveforms: a small compiled expression of time (`5*sin(2*pi*1k*t)`), CSV
//! import into piecewise-linear points, and the number formatting that writes points back as text.

use std::fmt;

// ---------------------------------------------------------------- expressions

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fun {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Ln,
    Log10,
    Sqrt,
    Abs,
    Sign,
    Floor,
    Ceil,
    Round,
    Min,
    Max,
    Pow,
    Mod,
    Frac,
    Limit,
    If,
    U,
    Ramp,
    Sqr,
    Tri,
    Saw,
    Pulse,
    Noise,
}

/// Name, function and how many arguments it takes (min, max).
const FUNS: &[(&str, Fun, usize, usize)] = &[
    ("sin", Fun::Sin, 1, 1),
    ("cos", Fun::Cos, 1, 1),
    ("tan", Fun::Tan, 1, 1),
    ("asin", Fun::Asin, 1, 1),
    ("acos", Fun::Acos, 1, 1),
    ("atan", Fun::Atan, 1, 1),
    ("atan2", Fun::Atan2, 2, 2),
    ("sinh", Fun::Sinh, 1, 1),
    ("cosh", Fun::Cosh, 1, 1),
    ("tanh", Fun::Tanh, 1, 1),
    ("exp", Fun::Exp, 1, 1),
    ("ln", Fun::Ln, 1, 1),
    ("log", Fun::Ln, 1, 1),
    ("log10", Fun::Log10, 1, 1),
    ("sqrt", Fun::Sqrt, 1, 1),
    ("abs", Fun::Abs, 1, 1),
    ("sign", Fun::Sign, 1, 1),
    ("sgn", Fun::Sign, 1, 1),
    ("floor", Fun::Floor, 1, 1),
    ("int", Fun::Floor, 1, 1),
    ("ceil", Fun::Ceil, 1, 1),
    ("round", Fun::Round, 1, 1),
    ("min", Fun::Min, 2, 16),
    ("max", Fun::Max, 2, 16),
    ("pow", Fun::Pow, 2, 2),
    ("pwr", Fun::Pow, 2, 2),
    ("mod", Fun::Mod, 2, 2),
    ("frac", Fun::Frac, 1, 1),
    ("limit", Fun::Limit, 3, 3),
    ("clamp", Fun::Limit, 3, 3),
    ("if", Fun::If, 3, 3),
    ("u", Fun::U, 1, 1),
    ("step", Fun::U, 1, 1),
    ("ramp", Fun::Ramp, 1, 1),
    ("uramp", Fun::Ramp, 1, 1),
    ("sqr", Fun::Sqr, 1, 1),
    ("square", Fun::Sqr, 1, 1),
    ("tri", Fun::Tri, 1, 1),
    ("saw", Fun::Saw, 1, 1),
    ("pulse", Fun::Pulse, 2, 2),
    ("noise", Fun::Noise, 1, 2),
];

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Num(f64),
    T,
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(Fun, Vec<Node>),
    Cond(Box<Node>, Box<Node>, Box<Node>),
}

/// A compiled expression of the time `t` in seconds.
#[derive(Clone)]
pub struct Expr {
    pub src: String,
    node: Node,
}

impl fmt::Debug for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Expr({})", self.src)
    }
}

impl PartialEq for Expr {
    fn eq(&self, o: &Self) -> bool {
        self.src == o.src
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(&'static str),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') {
                let j = if c.get(i + 1).is_some_and(|d| *d == '+' || *d == '-') { i + 2 } else { i + 1 };
                if c.get(j).is_some_and(|d| d.is_ascii_digit()) {
                    i = j;
                    while i < c.len() && c[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            // an SI prefix and unit (1k, 10m, 2.2u, 50Hz, 4k7)
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == 'µ' || c[i] == 'Ω') {
                i += 1;
            }
            let t: String = c[st..i].iter().collect();
            let v = crate::circuit::parse_si(&t).ok_or_else(|| format!("{t} is not a number (write 2*t, not 2t)"))?;
            out.push(Tok::Num(v));
            continue;
        }
        if ch == 'π' {
            out.push(Tok::Id("pi".into()));
            i += 1;
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect::<String>().to_ascii_lowercase()));
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let op2 = ["**", "<=", ">=", "==", "!=", "&&", "||"].into_iter().find(|o| *o == two);
        if let Some(o) = op2 {
            out.push(Tok::Op(if o == "**" { "^" } else { o }));
            i += 2;
            continue;
        }
        let op1 = ["+", "-", "*", "/", "%", "^", "(", ")", ",", "<", ">", "!", "?", ":"].into_iter().find(|o| o.starts_with(ch));
        match op1 {
            Some(o) => out.push(Tok::Op(o)),
            None => return Err(format!("unexpected '{ch}'")),
        }
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek_op(&self) -> Option<&'static str> {
        match self.toks.get(self.i) {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn eat(&mut self, o: &str) -> bool {
        if self.peek_op() == Some(o) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, o: &str) -> Result<(), String> {
        if self.eat(o) { Ok(()) } else { Err(format!("expected '{o}'")) }
    }

    fn cond(&mut self) -> Result<Node, String> {
        let c = self.bin(0)?;
        if self.eat("?") {
            let a = self.cond()?;
            self.expect(":")?;
            let b = self.cond()?;
            return Ok(Node::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn bin(&mut self, min: u8) -> Result<Node, String> {
        let mut a = self.unary()?;
        loop {
            let Some(o) = self.peek_op() else { break };
            let (op, p) = match o {
                "||" => (Op::Or, 1),
                "&&" => (Op::And, 2),
                "==" => (Op::Eq, 3),
                "!=" => (Op::Ne, 3),
                "<" => (Op::Lt, 4),
                "<=" => (Op::Le, 4),
                ">" => (Op::Gt, 4),
                ">=" => (Op::Ge, 4),
                "+" => (Op::Add, 5),
                "-" => (Op::Sub, 5),
                "*" => (Op::Mul, 6),
                "/" => (Op::Div, 6),
                "%" => (Op::Rem, 6),
                _ => break,
            };
            if p < min {
                break;
            }
            self.i += 1;
            let b = self.bin(p + 1)?;
            a = Node::Bin(op, Box::new(a), Box::new(b));
        }
        Ok(a)
    }

    fn unary(&mut self) -> Result<Node, String> {
        if self.eat("-") {
            return Ok(Node::Neg(Box::new(self.unary()?)));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok(Node::Not(Box::new(self.unary()?)));
        }
        self.power()
    }

    fn power(&mut self) -> Result<Node, String> {
        let a = self.atom()?;
        if self.eat("^") {
            // right associative, and binds tighter than a leading minus: -2^2 = -4
            let b = self.unary()?;
            return Ok(Node::Bin(Op::Pow, Box::new(a), Box::new(b)));
        }
        Ok(a)
    }

    fn atom(&mut self) -> Result<Node, String> {
        match self.toks.get(self.i).cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(Node::Num(v))
            }
            Some(Tok::Id(id)) => {
                self.i += 1;
                if self.eat("(") {
                    let Some(&(_, f, lo, hi)) = FUNS.iter().find(|x| x.0 == id) else {
                        return Err(format!("no function {id}()"));
                    };
                    let mut args = vec![];
                    if !self.eat(")") {
                        loop {
                            args.push(self.cond()?);
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    if args.len() < lo || args.len() > hi {
                        return Err(if lo == hi {
                            format!("{id}() takes {lo} argument{}", if lo == 1 { "" } else { "s" })
                        } else {
                            format!("{id}() takes {lo} to {hi} arguments")
                        });
                    }
                    return Ok(Node::Call(f, args));
                }
                match id.as_str() {
                    "t" | "time" => Ok(Node::T),
                    "pi" => Ok(Node::Num(std::f64::consts::PI)),
                    "e" => Ok(Node::Num(std::f64::consts::E)),
                    _ => Err(format!("unknown name {id} (use t for time)")),
                }
            }
            Some(Tok::Op("(")) => {
                self.i += 1;
                let n = self.cond()?;
                self.expect(")")?;
                Ok(n)
            }
            Some(Tok::Op(o)) => Err(format!("unexpected '{o}'")),
            None => Err("the expression ends too soon".into()),
        }
    }
}

fn b(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

fn frac(x: f64) -> f64 {
    x - x.floor()
}

/// White noise between −1 and 1, one value per whole `x`, joined by straight lines.
fn noise(x: f64, seed: f64) -> f64 {
    let h = |k: f64| {
        let mut z = (k as i64 as u64) ^ ((seed as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let k = x.floor();
    let f = x - k;
    h(k) + (h(k + 1.0) - h(k)) * f
}

fn eval(n: &Node, t: f64) -> f64 {
    match n {
        Node::Num(v) => *v,
        Node::T => t,
        Node::Neg(a) => -eval(a, t),
        Node::Not(a) => b(eval(a, t) == 0.0),
        Node::Bin(op, a, c) => {
            let x = eval(a, t);
            // && and || short-circuit, like if()
            match op {
                Op::And => return b(x != 0.0 && eval(c, t) != 0.0),
                Op::Or => return b(x != 0.0 || eval(c, t) != 0.0),
                _ => {}
            }
            let y = eval(c, t);
            match op {
                Op::Add => x + y,
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div => x / y,
                Op::Rem => x.rem_euclid(y),
                Op::Pow => x.powf(y),
                Op::Lt => b(x < y),
                Op::Le => b(x <= y),
                Op::Gt => b(x > y),
                Op::Ge => b(x >= y),
                Op::Eq => b(x == y),
                Op::Ne => b(x != y),
                Op::And | Op::Or => unreachable!(),
            }
        }
        Node::Cond(c, a, e) => {
            if eval(c, t) != 0.0 {
                eval(a, t)
            } else {
                eval(e, t)
            }
        }
        Node::Call(f, args) => {
            if *f == Fun::If {
                return if eval(&args[0], t) != 0.0 { eval(&args[1], t) } else { eval(&args[2], t) };
            }
            let a = |k: usize| eval(&args[k], t);
            let x = a(0);
            match f {
                Fun::Sin => x.sin(),
                Fun::Cos => x.cos(),
                Fun::Tan => x.tan(),
                Fun::Asin => x.asin(),
                Fun::Acos => x.acos(),
                Fun::Atan => x.atan(),
                Fun::Atan2 => x.atan2(a(1)),
                Fun::Sinh => x.sinh(),
                Fun::Cosh => x.cosh(),
                Fun::Tanh => x.tanh(),
                Fun::Exp => x.exp(),
                Fun::Ln => x.ln(),
                Fun::Log10 => x.log10(),
                Fun::Sqrt => x.sqrt(),
                Fun::Abs => x.abs(),
                Fun::Sign => {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                }
                Fun::Floor => x.floor(),
                Fun::Ceil => x.ceil(),
                Fun::Round => x.round(),
                Fun::Min => (1..args.len()).fold(x, |m, k| m.min(a(k))),
                Fun::Max => (1..args.len()).fold(x, |m, k| m.max(a(k))),
                Fun::Pow => x.powf(a(1)),
                Fun::Mod => x.rem_euclid(a(1)),
                Fun::Frac => frac(x),
                Fun::Limit => {
                    let (lo, hi) = (a(1), a(2));
                    x.max(lo.min(hi)).min(hi.max(lo))
                }
                Fun::U => b(x >= 0.0),
                Fun::Ramp => x.max(0.0),
                Fun::Sqr => {
                    if frac(x) < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                Fun::Tri => 1.0 - 4.0 * (frac(x + 0.25) - 0.5).abs(),
                Fun::Saw => 2.0 * frac(x + 0.5) - 1.0,
                Fun::Pulse => b(frac(x) < a(1)),
                Fun::Noise => noise(x, if args.len() > 1 { a(1) } else { 0.0 }),
                Fun::If => unreachable!(),
            }
        }
    }
}

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, String> {
        let toks = lex(src)?;
        if toks.is_empty() {
            return Err("the expression is empty".into());
        }
        let mut p = Parser { toks, i: 0 };
        let node = p.cond()?;
        if p.i < p.toks.len() {
            return Err(match &p.toks[p.i] {
                Tok::Op(o) => format!("unexpected '{o}'"),
                Tok::Num(v) => format!("unexpected number {v} (a * missing?)"),
                Tok::Id(id) => format!("unexpected {id} (a * missing?)"),
            });
        }
        Ok(Expr { src: src.trim().to_string(), node })
    }

    pub fn at(&self, t: f64) -> f64 {
        let v = eval(&self.node, t);
        if v.is_finite() { v } else { 0.0 }
    }

    /// Whether the value changes with time at all.
    pub fn uses_t(&self) -> bool {
        fn has(n: &Node) -> bool {
            match n {
                Node::T => true,
                Node::Num(_) => false,
                Node::Neg(a) | Node::Not(a) => has(a),
                Node::Bin(_, a, b) => has(a) || has(b),
                Node::Cond(a, b, c) => has(a) || has(b) || has(c),
                Node::Call(_, args) => args.iter().any(has),
            }
        }
        has(&self.node)
    }

    /// A step that follows the wave over 0..stop: a fortieth of the shortest swing about its
    /// middle seen in 4000 samples (a 50 Hz sine over 100 ms: 0.5 ms).
    pub fn auto_step(&self, stop: f64) -> f64 {
        if !self.uses_t() || !(stop > 0.0) {
            return f64::INFINITY;
        }
        const N: usize = 4000;
        let v: Vec<f64> = (0..=N).map(|k| self.at(stop * k as f64 / N as f64)).collect();
        let (lo, hi) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), x| (l.min(*x), h.max(*x)));
        if !(hi > lo) {
            return f64::INFINITY;
        }
        let mid = (lo + hi) / 2.0;
        let crossings = v.windows(2).filter(|w| (w[0] - mid) * (w[1] - mid) < 0.0).count();
        if crossings < 2 {
            // a ramp or one step: just keep the default number of points
            return f64::INFINITY;
        }
        // half a period between crossings; 40 steps a period, but no finer than the sampling
        (stop / crossings as f64 / 20.0).max(stop / N as f64 / 2.0)
    }
}

// ---------------------------------------------------------------- number text

/// A number in as few characters as round-trips through `parse_si` to nine digits: 1m, 2.5u, 0, -3.3.
pub fn num(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return "0".into();
    }
    let e3 = ((v.abs().log10() / 3.0).floor() as i32).clamp(-5, 4);
    let (pre, scale) = match e3 {
        -5 => ("f", 1e-15),
        -4 => ("p", 1e-12),
        -3 => ("n", 1e-9),
        -2 => ("u", 1e-6),
        -1 => ("m", 1e-3),
        0 => ("", 1.0),
        1 => ("k", 1e3),
        2 => ("Meg", 1e6),
        3 => ("G", 1e9),
        _ => ("T", 1e12),
    };
    let m = v / scale;
    // nine significant digits, trailing zeros trimmed
    let digits = (9 - (m.abs().log10().floor() as i32 + 1)).clamp(0, 9) as usize;
    let s = format!("{:.*}", digits, m);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    format!("{s}{pre}")
}

/// `PWL(t1 v1 t2 v2 …)` with an optional delay and the time it repeats from.
pub fn pwl_text(pts: &[(f64, f64)], td: f64, rep: Option<f64>) -> String {
    let body: Vec<String> = pts.iter().map(|(t, v)| format!("{} {}", num(*t), num(*v))).collect();
    let mut s = format!("PWL({})", body.join(" "));
    if let Some(r) = rep {
        s += &format!(" R={}", num(r));
    }
    if td != 0.0 {
        s += &format!(" TD={}", num(td));
    }
    s
}

// ---------------------------------------------------------------- CSV

/// Columns of numbers read from a CSV, TSV or space-separated file (a scope or generator export,
/// a spreadsheet): rows that are not all numbers (headers, comments) are skipped. Semicolon or
/// tab separated files may use decimal commas.
pub fn csv_columns(text: &str) -> Result<Vec<Vec<f64>>, String> {
    let mut rows: Vec<Vec<f64>> = vec![];
    for line in text.lines() {
        let l = line.trim().trim_start_matches('\u{feff}');
        if l.is_empty() || l.starts_with('#') || l.starts_with("//") {
            continue;
        }
        let fields: Vec<String> = if l.contains(';') {
            l.split(';').map(|f| f.trim().replace(',', ".")).collect()
        } else if l.contains('\t') {
            l.split('\t').map(|f| f.trim().replace(',', ".")).collect()
        } else if l.contains(',') {
            l.split(',').map(|f| f.trim().to_string()).collect()
        } else {
            l.split_whitespace().map(|f| f.to_string()).collect()
        };
        let fields: Vec<&str> = fields.iter().map(|f| f.trim_matches('"').trim()).collect();
        // trailing empty fields (a trailing separator) are not columns
        let n = fields.iter().rposition(|f| !f.is_empty()).map(|k| k + 1).unwrap_or(0);
        let vals: Option<Vec<f64>> = fields[..n].iter().map(|f| f.parse::<f64>().ok().or_else(|| crate::circuit::parse_si(f))).collect();
        if let Some(v) = vals
            && v.len() >= 2
        {
            rows.push(v);
        }
    }
    if rows.is_empty() {
        return Err("no rows of numbers (time, value) in the file".into());
    }
    let ncol = rows.iter().map(|r| r.len()).min().unwrap_or(0);
    Ok((0..ncol).map(|c| rows.iter().map(|r| r[c]).collect()).collect())
}

/// Time and value columns as PWL points: times start at 0 (a scope's pre-trigger samples move
/// up), repeated times keep their last value, and points on a straight line between their
/// neighbours (within a millionth of the value range) are dropped. Returns the points and a note
/// on what changed.
pub fn csv_points(cols: &[Vec<f64>], col: usize) -> Result<(Vec<(f64, f64)>, String), String> {
    if cols.len() < 2 {
        return Err("the file needs a time column and a value column".into());
    }
    let col = col.clamp(1, cols.len() - 1);
    let (ts, vs) = (&cols[0], &cols[col]);
    let mut pts: Vec<(f64, f64)> = vec![];
    for (t, v) in ts.iter().zip(vs) {
        if !t.is_finite() || !v.is_finite() {
            continue;
        }
        if let Some(last) = pts.last_mut() {
            if *t < last.0 {
                return Err(format!("times must increase: {} comes after {}", num(*t), num(last.0)));
            }
            if *t == last.0 {
                last.1 = *v;
                continue;
            }
        }
        pts.push((*t, *v));
    }
    if pts.len() < 2 {
        return Err("fewer than two points".into());
    }
    let mut notes = vec![];
    let t0 = pts[0].0;
    if t0 != 0.0 {
        for p in &mut pts {
            p.0 -= t0;
        }
        notes.push(format!("times moved by {}s to start at 0", num(-t0)));
    }
    let n0 = pts.len();
    let (lo, hi) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), p| (l.min(p.1), h.max(p.1)));
    let tol = ((hi - lo) * 1e-6).max(1e-15);
    let mut keep: Vec<(f64, f64)> = vec![pts[0]];
    for k in 1..pts.len() - 1 {
        let a = *keep.last().unwrap();
        let (b, c) = (pts[k], pts[k + 1]);
        let on_line = a.1 + (c.1 - a.1) * (b.0 - a.0) / (c.0 - a.0);
        if (b.1 - on_line).abs() > tol {
            keep.push(b);
        }
    }
    keep.push(*pts.last().unwrap());
    if keep.len() < n0 {
        notes.push(format!("{} of {n0} points kept (the rest lie on straight lines)", keep.len()));
    }
    Ok((keep, notes.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str, t: f64) -> f64 {
        Expr::parse(s).unwrap_or_else(|e| panic!("{s}: {e}")).at(t)
    }

    #[test]
    fn expressions() {
        assert_eq!(ev("1 + 2 * 3", 0.0), 7.0);
        assert_eq!(ev("-2^2", 0.0), -4.0);
        assert_eq!(ev("2^3^2", 0.0), 512.0);
        assert_eq!(ev("2**3", 0.0), 8.0);
        assert!((ev("5*sin(2*pi*1k*t)", 0.25e-3) - 5.0).abs() < 1e-12);
        assert!((ev("10m + 1k*t", 1e-3) - 1.01).abs() < 1e-12);
        assert_eq!(ev("t > 1m ? 5 : 0", 2e-3), 5.0);
        assert_eq!(ev("if(t < 1m, 1, 2)", 0.0), 1.0);
        assert_eq!(ev("u(t - 1m) * 3", 0.5e-3), 0.0);
        assert_eq!(ev("sqr(1k*t)", 0.2e-3), 1.0);
        assert_eq!(ev("sqr(1k*t)", 0.7e-3), -1.0);
        assert!((ev("tri(t)", 0.25) - 1.0).abs() < 1e-12);
        assert!(ev("tri(t)", 0.0).abs() < 1e-12);
        assert!((ev("saw(t)", 0.25) - 0.5).abs() < 1e-12);
        assert_eq!(ev("pulse(t, 0.25)", 0.2), 1.0);
        assert_eq!(ev("pulse(t, 0.25)", 0.3), 0.0);
        assert_eq!(ev("max(1, 4, 2)", 0.0), 4.0);
        assert_eq!(ev("limit(7, 0, 5)", 0.0), 5.0);
        assert_eq!(ev("t && 0", 1.0), 0.0);
        assert_eq!(ev("π", 0.0), std::f64::consts::PI);
        assert_eq!(ev("50Hz", 0.0), 50.0);
        let n = Expr::parse("noise(1k*t)").unwrap();
        assert!((0..1000).map(|k| n.at(k as f64 * 1.3e-4)).all(|v| (-1.0..=1.0).contains(&v)));
        assert_eq!(n.at(0.0123), n.at(0.0123));
        assert!(Expr::parse("2t").is_err());
        assert!(Expr::parse("sin(t").is_err());
        assert!(Expr::parse("foo(t)").is_err());
        assert!(Expr::parse("x + 1").is_err());
        assert!(Expr::parse("sin(1, 2)").is_err());
        // a step that follows the wave
        let s = Expr::parse("sin(2*pi*50*t)").unwrap().auto_step(0.1);
        assert!(s > 1e-4 && s < 1e-3, "{s}");
        assert_eq!(Expr::parse("3").unwrap().auto_step(1.0), f64::INFINITY);
    }

    #[test]
    fn numbers_round_trip() {
        for v in [0.0, 1.0, -3.3, 1e-3, 2.5e-6, 1e-9, 4.7e3, 1.5e6, 123.456789, 1.000001234, 1e-15, 0.1 + 0.2] {
            let s = num(v);
            let back = crate::circuit::parse_si(&s).unwrap();
            assert!((back - v).abs() <= v.abs() * 1e-8, "{v} -> {s} -> {back}");
        }
        assert_eq!(num(1e-3), "1m");
        assert_eq!(num(2.5e-6), "2.5u");
        assert_eq!(num(1.5e6), "1.5Meg");
        assert_eq!(pwl_text(&[(0.0, 0.0), (1e-3, 5.0)], 0.0, Some(0.0)), "PWL(0 0 1m 5) R=0");
    }

    #[test]
    fn csv_import() {
        let c = csv_columns("Time,CH1,CH2\n# comment\n-1e-3,0,1\n0,1,2\n1e-3,2,3\n2e-3,3,3\n3e-3,0,3\n").unwrap();
        assert_eq!(c.len(), 3);
        let (p, note) = csv_points(&c, 1).unwrap();
        // 0..3 is a straight line through the middle point: dropped
        let want = [(0.0, 0.0), (3e-3, 3.0), (4e-3, 0.0)];
        assert_eq!(p.len(), 3);
        assert!(p.iter().zip(want).all(|(a, b)| (a.0 - b.0).abs() < 1e-15 && (a.1 - b.1).abs() < 1e-12), "{p:?}");
        assert!(note.contains("start at 0") && note.contains("3 of 5"), "{note}");
        let (p2, _) = csv_points(&c, 2).unwrap();
        assert_eq!(p2.first(), Some(&(0.0, 1.0)));
        // decimal commas with semicolons, and tabs
        let e = csv_columns("t;v\n0;0,5\n0,001;1,5\n").unwrap();
        assert_eq!(e[1], vec![0.5, 1.5]);
        let tab = csv_columns("0\t1\n1m\t2\n").unwrap();
        assert_eq!(tab[0], vec![0.0, 1e-3]);
        assert!(csv_points(&csv_columns("1,0\n0,1\n").unwrap(), 1).is_err());
        assert!(csv_columns("a,b\n").is_err());
    }
}
