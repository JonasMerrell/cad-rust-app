//! Built setups (toolpaths and the stock they leave) kept on disk, so a setup built once isn't generated
//! again by the next session that opens the same document: a `verify_gcode` run after the CAM run, say.
//!
//! An entry is under a key that hashes the app version, a format number and everything a build depends
//! on: the setups of its chain as JSON, what each of them machines (the part's triangles, circles and
//! sketches) and the stock it starts from. Any change to one of them gives another key, so a stale entry is
//! never used.
//!
//! Everything is kept bit for bit: the toolpaths as their serde value tree with every number as its raw bits
//! (JSON text doesn't do: serde_json reads some numbers back a bit off, -51.519999999999996 as -51.52), the
//! stock as its raw bits. Toolpaths with a number JSON can't hold (NaN, infinity) aren't kept.
//!
//! Entries go in a `cam-build` folder of the build cache (`build_cache::dirs_for`): the project's
//! `.rcad-cache/` or the user's cache folder. Each folder is trimmed to [`LIMIT`], least recently used
//! first. `RCAD_NO_BUILD_CACHE=1` turns this off too.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::cam_paths::Toolpath;
use crate::cam_stock::StockMap;

/// Bump when the toolpaths change in a way the app version doesn't capture.
const FORMAT: u32 = 1;
/// The folder inside a build-cache folder.
pub const SUB: &str = "cam-build";
const EXT: &str = "build.gz";
const MAGIC: &[u8; 8] = b"RCADBLD1";
/// Most one folder may hold before the least recently used entries go.
const LIMIT: u64 = 1 << 30;

/// The folders for a document at `doc` (None: not saved yet); the first is written to.
pub fn dirs_for(doc: Option<&Path>) -> Vec<PathBuf> {
    crate::build_cache::dirs_for(doc).into_iter().map(|d| d.join(SUB)).collect()
}

/// FNV-1a, 64 bit: stable across runs, builds and platforms (unlike `DefaultHasher`).
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        std::hash::Hasher::write(&mut h, format!("rcad {} cam build {FORMAT}\n", env!("CARGO_PKG_VERSION")).as_bytes());
        h
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
        self.0.max(1)
    }
}

/// Hash a stock map's grid and every column, bit for bit.
pub fn hash_stock(h: &mut Fnv, m: Option<&StockMap>) {
    use std::hash::Hasher;
    let Some(m) = m else {
        h.write(b"block");
        return;
    };
    h.write(b"stock");
    for x in [m.x0, m.y0, m.cell] {
        h.write(&x.to_bits().to_le_bytes());
    }
    h.write(&(m.nx as u64).to_le_bytes());
    h.write(&(m.ny as u64).to_le_bytes());
    for x in m.hi.iter().chain(&m.lo) {
        h.write(&x.to_bits().to_le_bytes());
    }
}

fn file_name(key: u64) -> String {
    format!("{key:016x}.{EXT}")
}

/// The toolpaths and the stock after them kept under `key`, if a folder has them.
pub fn load(dirs: &[PathBuf], key: u64) -> Option<(Vec<Toolpath>, StockMap)> {
    let name = file_name(key);
    for p in dirs.iter().map(|d| d.join(&name)).filter(|p| p.is_file()) {
        let Some(got) = read(&p, key) else { continue };
        // the most recently used go last when the folder is trimmed
        if let Ok(f) = std::fs::File::options().append(true).open(&p) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Some(got);
    }
    None
}

fn read(path: &Path, key: u64) -> Option<(Vec<Toolpath>, StockMap)> {
    let file = std::fs::File::open(path).ok()?;
    let mut b = vec![];
    flate2::read::GzDecoder::new(std::io::BufReader::new(file)).read_to_end(&mut b).ok()?;
    decode(&b, key)
}

fn decode(b: &[u8], key: u64) -> Option<(Vec<Toolpath>, StockMap)> {
    let mut r = Reader(b);
    if r.take(8)? != MAGIC || r.u64()? != key {
        return None;
    }
    let paths: Vec<Toolpath> = serde_json::from_value(value(&mut r, 0)?).ok()?;
    let (x0, y0, cell) = (f64::from_bits(r.u64()?), f64::from_bits(r.u64()?), f64::from_bits(r.u64()?));
    let (nx, ny) = (r.u64()? as usize, r.u64()? as usize);
    let cells = nx.checked_mul(ny)?;
    let col = |r: &mut Reader| -> Option<Vec<f32>> { (0..cells).map(|_| r.u32().map(f32::from_bits)).collect() };
    let hi = col(&mut r)?;
    let lo = col(&mut r)?;
    r.0.is_empty().then_some((paths, StockMap { x0, y0, cell, nx, ny, hi, lo }))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        (self.0.len() >= n).then(|| {
            let (a, b) = self.0.split_at(n);
            self.0 = b;
            a
        })
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u64()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Whether every number in `v` is finite: serde's value tree (like JSON) holds NaN and infinity as null, so
/// they wouldn't come back. Walks `v` as serde sees it, without building anything.
fn all_finite<T: serde::Serialize + ?Sized>(v: &T) -> bool {
    v.serialize(&mut Finite).is_ok()
}

struct Finite;

#[derive(Debug)]
struct NotFinite;

impl std::fmt::Display for NotFinite {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a number that isn't finite")
    }
}

impl std::error::Error for NotFinite {}

impl serde::ser::Error for NotFinite {
    fn custom<M: std::fmt::Display>(_: M) -> Self {
        NotFinite
    }
}

type F = Result<(), NotFinite>;

impl serde::Serializer for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    fn serialize_bool(self, _: bool) -> F {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> F {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> F {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> F {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> F {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> F {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> F {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> F {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> F {
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> F {
        if v.is_finite() { Ok(()) } else { Err(NotFinite) }
    }
    fn serialize_f64(self, v: f64) -> F {
        if v.is_finite() { Ok(()) } else { Err(NotFinite) }
    }
    fn serialize_char(self, _: char) -> F {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> F {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> F {
        Ok(())
    }
    fn serialize_none(self) -> F {
        Ok(())
    }
    fn serialize_some<T: serde::Serialize + ?Sized>(self, v: &T) -> F {
        v.serialize(self)
    }
    fn serialize_unit(self) -> F {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> F {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> F {
        Ok(())
    }
    fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(self, _: &'static str, v: &T) -> F {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(self, _: &'static str, _: u32, _: &'static str, v: &T) -> F {
        v.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, NotFinite> {
        Ok(self)
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, NotFinite> {
        Ok(self)
    }
}

impl serde::ser::SerializeSeq for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_element<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_element<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeMap for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_key<T: serde::Serialize + ?Sized>(&mut self, k: &T) -> F {
        k.serialize(&mut **self)
    }
    fn serialize_value<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for &mut Finite {
    type Ok = ();
    type Error = NotFinite;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> F {
        v.serialize(&mut **self)
    }
    fn end(self) -> F {
        Ok(())
    }
}

/// A serde value tree, numbers as their bits.
fn put_value(b: &mut Vec<u8>, v: &serde_json::Value) {
    use serde_json::Value;
    let s = |b: &mut Vec<u8>, s: &str| {
        b.extend_from_slice(&(s.len() as u64).to_le_bytes());
        b.extend_from_slice(s.as_bytes());
    };
    match v {
        Value::Null => b.push(0),
        Value::Bool(x) => b.push(1 + *x as u8),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                b.push(3);
                b.extend_from_slice(&u.to_le_bytes());
            } else if let Some(i) = n.as_i64() {
                b.push(4);
                b.extend_from_slice(&i.to_le_bytes());
            } else {
                b.push(5);
                b.extend_from_slice(&n.as_f64().unwrap_or(f64::NAN).to_bits().to_le_bytes());
            }
        }
        Value::String(x) => {
            b.push(6);
            s(b, x);
        }
        Value::Array(a) => {
            b.push(7);
            b.extend_from_slice(&(a.len() as u64).to_le_bytes());
            a.iter().for_each(|x| put_value(b, x));
        }
        Value::Object(m) => {
            b.push(8);
            b.extend_from_slice(&(m.len() as u64).to_le_bytes());
            for (k, x) in m {
                s(b, k);
                put_value(b, x);
            }
        }
    }
}

fn value(r: &mut Reader, depth: usize) -> Option<serde_json::Value> {
    use serde_json::Value;
    if depth > 64 {
        return None;
    }
    Some(match r.take(1)?[0] {
        0 => Value::Null,
        1 => Value::Bool(false),
        2 => Value::Bool(true),
        3 => Value::from(r.u64()?),
        4 => Value::from(r.u64()? as i64),
        5 => Value::Number(serde_json::Number::from_f64(f64::from_bits(r.u64()?))?),
        6 => Value::String(r.str()?),
        7 => {
            let n = r.u64()? as usize;
            let mut a = Vec::with_capacity(n.min(r.0.len()));
            for _ in 0..n {
                a.push(value(r, depth + 1)?);
            }
            Value::Array(a)
        }
        8 => {
            let n = r.u64()? as usize;
            let mut m = serde_json::Map::new();
            for _ in 0..n {
                let k = r.str()?;
                m.insert(k, value(r, depth + 1)?);
            }
            Value::Object(m)
        }
        _ => return None,
    })
}

/// The bytes of an entry, or None when the toolpaths hold a number JSON can't (NaN or infinity: serde
/// writes them as null, so they wouldn't come back).
fn encode(key: u64, paths: &[Toolpath], after: &StockMap) -> Option<Vec<u8>> {
    if !all_finite(paths) {
        return None;
    }
    let v = serde_json::to_value(paths).ok()?;
    if after.hi.len() != after.nx * after.ny || after.lo.len() != after.hi.len() {
        return None;
    }
    let mut b = Vec::with_capacity(8 * after.hi.len() + 1024);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&key.to_le_bytes());
    put_value(&mut b, &v);
    for x in [after.x0, after.y0, after.cell] {
        b.extend_from_slice(&x.to_bits().to_le_bytes());
    }
    b.extend_from_slice(&(after.nx as u64).to_le_bytes());
    b.extend_from_slice(&(after.ny as u64).to_le_bytes());
    for x in after.hi.iter().chain(&after.lo) {
        b.extend_from_slice(&x.to_bits().to_le_bytes());
    }
    Some(b)
}

/// Keep a build under `key` in the first of `dirs`, now. Nothing is kept when it wouldn't come back exactly
/// (see the module notes) or the folder can't be written.
pub fn store(dirs: &[PathBuf], key: u64, paths: &[Toolpath], after: &StockMap) {
    let Some(dir) = dirs.first() else { return };
    let path = dir.join(file_name(key));
    if path.is_file() {
        return;
    }
    let Some(bytes) = encode(key, paths, after) else {
        log::info!("a CAM build isn't kept on disk: its toolpaths don't read back exactly");
        return;
    };
    if let Err(e) = write(dir, &path, key, &bytes) {
        log::warn!("could not keep a CAM build in {}: {e}", dir.display());
        return;
    }
    prune(dir, LIMIT);
}

fn write(dir: &Path, path: &Path, key: u64, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(&ignore, "# rCAD's built CAM setups, remade from the document when missing\n*\n");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(bytes)?;
    let data = gz.finish()?;
    // a name of its own, so two writers of the same entry don't trip over each other
    let tmp = dir.join(format!(".{key:016x}.{}.tmp", std::process::id()));
    let res = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Remove the least recently used entries of `dir` until it holds at most `limit` bytes.
fn prune(dir: &Path, limit: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(EXT))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, p) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam_paths::Move;

    fn sample() -> (Vec<Toolpath>, StockMap) {
        let tp = Toolpath {
            op: 7,
            tool: 3,
            // (-51.519999999999996 is one serde_json's JSON reader gets a bit wrong: it reads -51.52)
            moves: vec![Move::Rapid([0.1, 0.2 / 3.0, 25.0]), Move::Feed([-0.0, -51.519999999999996, 0.30000000000000004], 1350.0), Move::Dwell(3.0)],
            warnings: vec!["a warning".into()],
            cut_length: 12.345678901234567,
            rapid_length: 1e-7,
            time: 61.5,
            ..Default::default()
        };
        let m = StockMap { x0: -1.5, y0: 2.0 / 3.0, cell: 0.1, nx: 3, ny: 2, hi: vec![1.0, crate::cam_stock::EMPTY, 2.5, 0.1, 9.0, f32::MIN_POSITIVE], lo: vec![0.0; 6] };
        (vec![tp], m)
    }

    #[test]
    fn a_kept_build_comes_back_bit_for_bit() {
        let dir = std::env::temp_dir().join(format!("rcad-cam-build-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (paths, after) = sample();
        let dirs = vec![dir.clone()];
        assert!(load(&dirs, 42).is_none());
        store(&dirs, 42, &paths, &after);
        let (p2, a2) = load(&dirs, 42).expect("kept");
        assert_eq!(serde_json::to_string(&p2).unwrap(), serde_json::to_string(&paths).unwrap());
        let Move::Feed(p, _) = p2[0].moves[1] else { panic!() };
        assert_eq!(p.map(f64::to_bits), [(-0.0f64).to_bits(), (-51.519999999999996f64).to_bits(), 0.30000000000000004f64.to_bits()]);
        assert_eq!(crate::cam_verify::toolpath_hash(&p2), crate::cam_verify::toolpath_hash(&paths));
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!((a2.x0.to_bits(), a2.y0.to_bits(), a2.cell.to_bits(), a2.nx, a2.ny), (after.x0.to_bits(), after.y0.to_bits(), after.cell.to_bits(), after.nx, after.ny));
        assert_eq!((bits(&a2.hi), bits(&a2.lo)), (bits(&after.hi), bits(&after.lo)));
        // another key, a damaged entry and one of another key under this name are misses
        assert!(load(&dirs, 43).is_none());
        let p = dir.join(file_name(42));
        std::fs::write(&p, b"not gzip").unwrap();
        assert!(load(&dirs, 42).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_entries_are_refused() {
        let (paths, after) = sample();
        let b = encode(5, &paths, &after).unwrap();
        assert!(decode(&b, 5).is_some());
        assert!(decode(&b, 6).is_none());
        for n in [0, 8, 20, b.len() - 1] {
            assert!(decode(&b[..n], 5).is_none(), "{n} bytes");
        }
        let mut longer = b.clone();
        longer.push(0);
        assert!(decode(&longer, 5).is_none());
    }

    #[test]
    fn options_left_out_are_kept_and_come_back_as_none() {
        // a null that stands for None (a probe step's feed) is kept; only numbers that aren't finite stop a build
        assert!(all_finite(&(Some(1.5), None::<f64>, vec![[0.0, -0.0]])));
        assert!(!all_finite(&(Some(f64::INFINITY), 1)));
        assert!(!all_finite(&vec![(String::from("x"), f32::NAN)]));
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct S {
            a: Option<f64>,
            b: Option<f64>,
            c: f64,
        }
        let s = vec![S { a: None, b: Some(-51.519999999999996), c: 0.1 + 0.2 }];
        let mut b = vec![];
        put_value(&mut b, &serde_json::to_value(&s).unwrap());
        let back: Vec<S> = serde_json::from_value(value(&mut Reader(&b), 0).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back[0].b.unwrap().to_bits(), (-51.519999999999996f64).to_bits());
    }

    #[test]
    fn a_build_that_does_not_read_back_exactly_is_not_kept() {
        let (mut paths, after) = sample();
        // JSON writes a non-finite number as null, which doesn't read back as a number
        paths[0].time = f64::NAN;
        assert!(encode(1, &paths, &after).is_none());
    }

    #[test]
    fn the_key_follows_the_stock() {
        use std::hash::Hasher;
        let (_, a) = sample();
        let mut b = a.clone();
        b.hi[2] = 2.4;
        let k = |m: Option<&StockMap>| {
            let mut h = Fnv::default();
            hash_stock(&mut h, m);
            h.finish()
        };
        assert_ne!(k(Some(&a)), k(Some(&b)));
        assert_ne!(k(Some(&a)), k(None));
        assert_eq!(k(Some(&a)), k(Some(&a.clone())));
    }
}
