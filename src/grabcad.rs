//! Bringing in models from GrabCAD (or any site that hands out STEP files and ZIPs of them).
//! GrabCAD has no public API and its downloads need a signed-in browser, so the app opens a
//! library search in the browser and then imports what lands in the Downloads folder: a STEP,
//! IGES, STL or OBJ file, or the ZIP GrabCAD packs them in.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The GrabCAD library search for `query`, limited to models with STEP/IGES files.
pub fn search_url(query: &str) -> String {
    let q: String = query
        .trim()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if q.is_empty() {
        "https://grabcad.com/library/software/step-slash-iges".into()
    } else {
        format!("https://grabcad.com/library/software/step-slash-iges?query={q}")
    }
}

/// A model file to import: its name and bytes.
pub struct ModelFile {
    pub name: String,
    pub format: &'static str,
    pub bytes: Vec<u8>,
}

/// What a ZIP or model file holds that can be imported.
pub struct Found {
    pub files: Vec<ModelFile>,
    /// Native CAD files that were skipped (SolidWorks, Inventor, …), for the message.
    pub skipped: Vec<String>,
}

fn rank(format: &str) -> usize {
    match format {
        "step" => 0,
        "parasolid" => 1,
        "iges" => 2,
        "stl" => 3,
        _ => 4,
    }
}

/// The model files at `path`: the file itself, or the best format found inside a ZIP (STEP over
/// Parasolid over IGES over STL over OBJ; every file of that format, since a download can hold
/// several parts). Parasolid files come back already turned into STEP.
pub fn read_models(path: &Path) -> Result<Found, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let is_zip = path.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false) || bytes.starts_with(b"PK\x03\x04");
    if !is_zip {
        let format = crate::import::format_of(path).ok_or_else(|| format!("{} is not a STEP, Parasolid, IGES, STL, OBJ or ZIP file", path.display()))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Import".into());
        let (format, bytes) = crate::import::storable(format, bytes, &name)?;
        return Ok(Found { files: vec![ModelFile { name, format, bytes }], skipped: vec![] });
    }
    let entries = unzip(&bytes)?;
    let mut files = vec![];
    let mut skipped = vec![];
    for (name, data) in entries {
        let p = Path::new(&name);
        // macOS resource forks and folders
        if name.ends_with('/') || name.contains("__MACOSX") || p.file_name().map(|f| f.to_string_lossy().starts_with("._")).unwrap_or(false) {
            continue;
        }
        if p.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false) {
            // a ZIP inside the ZIP (GrabCAD sometimes nests them)
            if let Ok(inner) = unzip(&data) {
                for (n, d) in inner {
                    if let Some(f) = crate::import::format_of(Path::new(&n)) {
                        files.push(ModelFile { name: stem(&n), format: f, bytes: d });
                    }
                }
            }
            continue;
        }
        match crate::import::format_of(p) {
            Some(f) => files.push(ModelFile { name: stem(&name), format: f, bytes: data }),
            None => {
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if matches!(
                    ext.as_str(),
                    "sldprt" | "sldasm" | "ipt" | "iam" | "f3d" | "prt" | "asm" | "catpart" | "catproduct" | "x_b" | "3dm" | "skp" | "par" | "fcstd" | "dwg" | "sat"
                ) {
                    skipped.push(p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(name.clone()));
                }
            }
        }
    }
    if let Some(best) = files.iter().map(|f| rank(f.format)).min() {
        files.retain(|f| rank(f.format) == best);
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    for f in &mut files {
        let (format, bytes) = crate::import::storable(f.format, std::mem::take(&mut f.bytes), &f.name).map_err(|e| format!("{}: {e}", f.name))?;
        f.format = format;
        f.bytes = bytes;
    }
    Ok(Found { files, skipped })
}

fn stem(name: &str) -> String {
    Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string())
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// The files in a ZIP archive (stored or deflated), read through its central directory.
pub fn unzip(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "the ZIP file is damaged or uses an unsupported format".to_string();
    // end of central directory: the last "PK\5\6" within the final 64 KiB
    let from = b.len().saturating_sub(65_557);
    let eocd = (from..b.len().saturating_sub(21)).rev().find(|&i| b[i..i + 4] == *b"PK\x05\x06").ok_or_else(bad)?;
    let count = u16_at(b, eocd + 10).ok_or_else(bad)?;
    let mut p = u32_at(b, eocd + 16).ok_or_else(bad)?;
    let mut out = vec![];
    for _ in 0..count {
        if b.get(p..p + 4) != Some(b"PK\x01\x02") {
            return Err(bad());
        }
        let method = u16_at(b, p + 10).ok_or_else(bad)?;
        let csize = u32_at(b, p + 20).ok_or_else(bad)?;
        let usize_ = u32_at(b, p + 24).ok_or_else(bad)?;
        let (nlen, xlen, clen) = (u16_at(b, p + 28).ok_or_else(bad)?, u16_at(b, p + 30).ok_or_else(bad)?, u16_at(b, p + 32).ok_or_else(bad)?);
        let local = u32_at(b, p + 42).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(b.get(p + 46..p + 46 + nlen).ok_or_else(bad)?).replace('\\', "/");
        p += 46 + nlen + xlen + clen;
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
            return Err("ZIP64 archives (over 4 GB) are not supported".into());
        }
        if b.get(local..local + 4) != Some(b"PK\x03\x04") {
            return Err(bad());
        }
        let start = local + 30 + u16_at(b, local + 26).ok_or_else(bad)? + u16_at(b, local + 28).ok_or_else(bad)?;
        let data = b.get(start..start + csize).ok_or_else(bad)?;
        let bytes = match method {
            0 => data.to_vec(),
            8 => {
                let mut v = Vec::with_capacity(usize_);
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut v).map_err(|e| format!("could not unpack {name}: {e}"))?;
                v
            }
            m => return Err(format!("{name} is compressed with method {m}, which is not supported")),
        };
        out.push((name, bytes));
    }
    Ok(out)
}

/// The user's Downloads folder.
pub fn downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    // XDG_DOWNLOAD_DIR from user-dirs.dirs, else ~/Downloads
    let home = PathBuf::from(home);
    if let Ok(s) = std::fs::read_to_string(home.join(".config/user-dirs.dirs")) {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let v = v.trim().trim_matches('"').replace("$HOME", &home.to_string_lossy());
                let p = PathBuf::from(v);
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
    }
    Some(home.join("Downloads")).filter(|p| p.is_dir())
}

/// Model files and ZIPs downloaded in the last `within`, newest first.
pub fn recent_downloads(within: Duration) -> Vec<(PathBuf, SystemTime)> {
    let Some(dir) = downloads_dir() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let now = SystemTime::now();
    let mut v: Vec<(PathBuf, SystemTime)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ok = p.extension().map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false) || crate::import::format_of(&p).is_some();
            let t = e.metadata().ok()?.modified().ok()?;
            (ok && p.is_file() && now.duration_since(t).map(|d| d <= within).unwrap_or(true)).then_some((p, t))
        })
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(12);
    v
}

/// "3 min ago" style age.
pub fn age(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_encodes() {
        assert_eq!(search_url("608 bearing"), "https://grabcad.com/library/software/step-slash-iges?query=608+bearing");
        assert_eq!(search_url("M5 & nut"), "https://grabcad.com/library/software/step-slash-iges?query=M5+%26+nut");
    }

    /// A tiny ZIP made by hand: one stored file and one deflated file.
    #[test]
    fn unzip_stored_and_deflated() {
        use std::io::Write;
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"ISO-10303-21; deflated").unwrap();
        let deflated = z.finish().unwrap();
        let files: Vec<(&str, u16, Vec<u8>, usize)> = vec![("a/part.step", 0, b"ISO-10303-21; stored".to_vec(), 20), ("b.STP", 8, deflated, 22), ("c.SLDPRT", 0, b"x".to_vec(), 1)];
        let mut out = vec![];
        let mut cd = vec![];
        for (name, method, data, usz) in &files {
            let off = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(*usz as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0; 8]);
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(*usz as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0; 12]);
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        let e = unzip(&out).unwrap();
        assert_eq!(e[0].1, b"ISO-10303-21; stored");
        assert_eq!(e[1].1, b"ISO-10303-21; deflated");
        let dir = std::env::temp_dir().join(format!("grabcad-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("model.zip");
        std::fs::write(&p, &out).unwrap();
        let f = read_models(&p).unwrap();
        assert_eq!(f.files.len(), 2);
        assert_eq!(f.files[0].name, "b");
        assert_eq!(f.skipped, vec!["c.SLDPRT".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
