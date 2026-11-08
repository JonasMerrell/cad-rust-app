//! McMaster-Carr parts in assemblies.
//!
//! McMaster-Carr's terms forbid scraping its site, so rCAD never fetches mcmaster.com pages
//! itself. There are two ways in:
//!
//! - **Browser.** The search opens mcmaster.com in the user's browser; the user picks a part,
//!   downloads its 3-D STEP and inserts it from the Downloads list. McMaster names its downloads
//!   `<part number>_<description>.STEP`, so the part number and description become the part's name.
//! - **Product Information API** (`api.mcmaster.com`), for approved customers only: McMaster gives
//!   each one a client certificate with its password, and an API user name and password. With
//!   those saved, a part number is enough: rCAD subscribes to the product, reads its description
//!   and fetches its 3-D STEP. The API has no search, only lookup by part number. The settings
//!   live in `~/.config/cad-rust-app/mcmaster.json` (owner-only) or come from `MCMASTER_USERNAME`,
//!   `MCMASTER_PASSWORD`, `MCMASTER_CERT` and `MCMASTER_CERT_PASSWORD`.
//!
//! McMaster's terms also say its CAD models are to help decide what to buy and must not be
//! shared with third parties (nor drawings or prototypes that include them), so parts that come
//! from McMaster go in Part Studios named `McMaster-Carr <part number>` and rCAD warns before a
//! project holding them is published or exported.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const API_URL: &str = "https://api.mcmaster.com";

/// The API's address (`RCAD_MCMASTER_API` points it at a test server).
fn api() -> String {
    env("RCAD_MCMASTER_API").unwrap_or_else(|| API_URL.to_string()).trim_end_matches('/').to_string()
}
/// Where McMaster explains the API and whom to ask for access.
pub const API_HELP: &str = "https://www.mcmaster.com/help/api";
pub const API_EMAIL: &str = "eCommerce@mcmaster.com";
pub const TERMS: &str = "https://www.mcmaster.com/termsandconditions";
/// The prefix of the Part Studios McMaster parts are imported into.
pub const STUDIO_PREFIX: &str = "McMaster-Carr ";

// ---------------------------------------------------------------- part numbers and names

/// Whether `s` is a McMaster-Carr part number: 4 or 5 digits, a capital letter, then 1 to 5
/// digits (91292A113, 6655K14, 47065T101), optionally with a dash suffix (5234K47-RD).
pub fn is_part_number(s: &str) -> bool {
    let (main, suffix) = match s.split_once('-') {
        Some((m, x)) => (m, Some(x)),
        None => (s, None),
    };
    if let Some(x) = suffix {
        if x.is_empty() || x.len() > 4 || !x.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
    }
    let b = main.as_bytes();
    let lead = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(4..=5).contains(&lead) || b.len() <= lead || !b[lead].is_ascii_uppercase() {
        return false;
    }
    let tail = &b[lead + 1..];
    (1..=5).contains(&tail.len()) && tail.iter().all(|c| c.is_ascii_digit())
}

/// The part number a McMaster download's file name starts with, and the description after it:
/// `91292A113_Black-Oxide Alloy Steel Socket Head Screw` → ("91292A113", "Black-Oxide Alloy Steel
/// Socket Head Screw"). `None` when the name doesn't start with a part number.
pub fn from_file_stem(stem: &str) -> Option<(String, String)> {
    let stem = stem.trim();
    let end = stem.find(|c: char| c == '_' || c == ' ').unwrap_or(stem.len());
    let pn = &stem[..end];
    if !is_part_number(pn) {
        return None;
    }
    let desc = stem[end..].trim_start_matches(['_', ' ']).replace('_', " ");
    Some((pn.to_string(), desc.split_whitespace().collect::<Vec<_>>().join(" ")))
}

/// The name an imported McMaster part gets: its part number, then a short description.
pub fn part_name(pn: &str, desc: &str) -> String {
    let d = desc.trim();
    if d.is_empty() { pn.to_string() } else { format!("{pn} {d}") }
}

pub fn studio_name(pn: &str) -> String {
    format!("{STUDIO_PREFIX}{pn}")
}

/// The part numbers of the McMaster parts in a workspace (from its Part Studio names).
pub fn parts_in(ws: &crate::workspace::Workspace) -> Vec<String> {
    let mut v: Vec<String> = ws.studios.iter().filter_map(|s| s.name.strip_prefix(STUDIO_PREFIX)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    v.sort();
    v.dedup();
    v
}

/// The warning shown before a project holding McMaster CAD is shared.
pub fn share_warning(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let list = if parts.len() > 4 { format!("{} and {} more", parts[..3].join(", "), parts.len() - 3) } else { parts.join(", ") };
    Some(format!(
        "This project has McMaster-Carr CAD ({list}). McMaster's terms don't allow sharing their models, or designs that include them, with others: keep it private, or delete those Part Studios before sharing."
    ))
}

// ---------------------------------------------------------------- the website

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The mcmaster.com page for a search: the product page for a part number, else the catalogue
/// search for the words.
pub fn search_url(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return "https://www.mcmaster.com/".into();
    }
    if is_part_number(&q.to_ascii_uppercase()) {
        return product_url(&q.to_ascii_uppercase());
    }
    let words: Vec<String> = q.split_whitespace().map(|w| encode(&w.to_lowercase())).collect();
    format!("https://www.mcmaster.com/products/{}/", words.join("-"))
}

pub fn product_url(pn: &str) -> String {
    format!("https://www.mcmaster.com/{}/", encode(pn.trim()))
}

// ---------------------------------------------------------------- API settings

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// The API user name and password McMaster set up (usually the mcmaster.com account's).
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// McMaster's client certificate: the .pfx/.p12 they send, or a PEM holding the
    /// certificate and its private key.
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub cert_password: String,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Settings {
    /// The saved settings, each empty field filled from the environment.
    pub fn effective(&self) -> Settings {
        let pick = |v: &str, k: &str| if v.trim().is_empty() { env(k).unwrap_or_default() } else { v.trim().to_string() };
        Settings {
            username: pick(&self.username, "MCMASTER_USERNAME"),
            password: if self.password.is_empty() { env("MCMASTER_PASSWORD").unwrap_or_default() } else { self.password.clone() },
            cert: pick(&self.cert, "MCMASTER_CERT"),
            cert_password: if self.cert_password.is_empty() { env("MCMASTER_CERT_PASSWORD").unwrap_or_default() } else { self.cert_password.clone() },
        }
    }
    pub fn has_creds(&self) -> bool {
        let e = self.effective();
        !e.username.is_empty() && !e.password.is_empty() && !e.cert.is_empty()
    }
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("cad-rust-app").join("mcmaster.json"))
}

pub fn load_settings() -> Settings {
    settings_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Save the settings (passwords included) readable by the user only.
pub fn save_settings(s: &Settings) -> Result<PathBuf, String> {
    let p = settings_path().ok_or("no config folder (HOME is not set)")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p).map_err(|e| e.to_string())?;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    *SESSION.lock().unwrap() = None;
    Ok(p)
}

// ---------------------------------------------------------------- the client certificate

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// The certificate chain and key in PEM text.
fn identity_from_pem(pem: &[u8]) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem).filter_map(|c| c.ok()).collect();
    if certs.is_empty() {
        return Err("the certificate file has no certificate in it".into());
    }
    let key = PrivateKeyDer::from_pem_slice(pem).map_err(|_| "the certificate file has no private key (a PEM needs both the certificate and its key)".to_string())?;
    Ok((certs, key))
}

/// A .pfx/.p12 as PEM, through the `openssl` command (rCAD has no PKCS#12 reader of its own).
fn pfx_to_pem(path: &Path, password: &str) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    // McMaster's certificates can use old ciphers that OpenSSL 3 only reads with -legacy
    for legacy in [false, true] {
        let mut cmd = std::process::Command::new("openssl");
        cmd.arg("pkcs12").arg("-in").arg(path).args(["-nodes", "-passin", "env:RCAD_MCMASTER_P12"]).env("RCAD_MCMASTER_P12", password);
        if legacy {
            cmd.arg("-legacy");
        }
        match cmd.output() {
            Ok(o) if o.status.success() && !o.stdout.is_empty() => return Ok(o.stdout),
            Ok(o) => last = String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("").trim().to_string(),
            Err(_) => {
                return Err(format!(
                    "reading a .pfx certificate needs the openssl command, which isn't installed. Convert it once with `openssl pkcs12 -in {} -out mcmaster.pem -nodes` on another computer and choose the .pem",
                    path.display()
                ));
            }
        }
    }
    Err(format!("openssl couldn't open {} (wrong certificate password?): {last}", path.display()))
}

fn tls_config(set: &Settings) -> Result<rustls::ClientConfig, String> {
    let path = PathBuf::from(&set.cert);
    let bytes = std::fs::read(&path).map_err(|e| format!("couldn't read the certificate {}: {e}", path.display()))?;
    let pem = if bytes.starts_with(b"-----") || bytes.windows(11).any(|w| w == b"-----BEGIN ") { bytes } else { pfx_to_pem(&path, &set.cert_password)? };
    let (certs, key) = identity_from_pem(&pem)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| format!("the client certificate can't be used: {e}"))
}

// ---------------------------------------------------------------- the API

struct Session {
    key: (String, String),
    agent: ureq::Agent,
    token: String,
    until: Instant,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn api_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => status_error(code, &r.into_string().unwrap_or_default()),
        ureq::Error::Transport(t) => {
            let t = t.to_string();
            if t.contains("certificate") || t.contains("handshake") || t.contains("alert") {
                format!("couldn't reach api.mcmaster.com: {t}. Check the client certificate McMaster gave you")
            } else {
                format!("couldn't reach api.mcmaster.com: {t}")
            }
        }
    }
}

fn status_error(code: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = [&v["ErrorDescription"], &v["ErrorMessage"]].iter().map(|x| s(x)).find(|x| !x.is_empty()).unwrap_or_else(|| body.chars().take(200).collect());
    match code {
        401 => format!("McMaster-Carr refused the login (401): {msg}. The account needs API access, which McMaster grants on request ({API_EMAIL})"),
        403 => format!("McMaster-Carr refused the request (403): {msg}"),
        404 => format!("McMaster-Carr has no such part (404): {msg}"),
        429 => "McMaster-Carr's rate limit was reached (429); try again later".into(),
        _ => format!("McMaster-Carr answered {code}: {msg}"),
    }
}

fn json_of(r: ureq::Response) -> Result<Value, String> {
    let t = r.into_string().map_err(|e| format!("McMaster-Carr's answer: {e}"))?;
    serde_json::from_str(&t).map_err(|e| format!("McMaster-Carr's answer is not JSON: {e}"))
}

/// A signed-in agent and bearer token, made once and reused until the token nearly expires
/// (McMaster's last 24 hours).
fn session(set: &Settings, fresh: bool) -> Result<(ureq::Agent, String), String> {
    let set = set.effective();
    if set.username.is_empty() || set.password.is_empty() {
        return Err("no McMaster-Carr API user name and password: add them under API settings".into());
    }
    if set.cert.is_empty() {
        return Err("no McMaster-Carr client certificate: choose the one McMaster sent you under API settings".into());
    }
    let key = (set.username.clone(), set.cert.clone());
    let mut guard = SESSION.lock().unwrap();
    if let Some(sess) = guard.as_ref() {
        if !fresh && sess.key == key && Instant::now() < sess.until {
            return Ok((sess.agent.clone(), sess.token.clone()));
        }
    }
    let agent = ureq::AgentBuilder::new()
        .tls_config(Arc::new(tls_config(&set)?))
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        // a local test server is reached directly
        .try_proxy_from_env(!api().starts_with("http://127.0.0.1"))
        .build();
    let r = agent.post(&format!("{}/v1/login", api())).set("Content-Type", "application/json").send_string(&json!({"UserName": set.username, "Password": set.password}).to_string()).map_err(api_error)?;
    let v = json_of(r)?;
    let token = s(&v["AuthToken"]);
    if token.is_empty() {
        return Err("McMaster-Carr gave no auth token".into());
    }
    *guard = Some(Session { key, agent: agent.clone(), token: token.clone(), until: Instant::now() + Duration::from_secs(23 * 3600) });
    Ok((agent, token))
}

/// Run a request, signing in again once if the token has expired.
fn call(set: &Settings, f: &dyn Fn(&ureq::Agent, &str) -> Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
    let (agent, token) = session(set, false)?;
    match f(&agent, &format!("Bearer {token}")) {
        Err(ureq::Error::Status(403, r)) => {
            let body = r.into_string().unwrap_or_default();
            if !body.contains("EXPIRED_AUTHORIZATION_TOKEN") && !body.contains("authorization token") {
                return Err(status_error(403, &body));
            }
            let (agent, token) = session(set, true)?;
            f(&agent, &format!("Bearer {token}")).map_err(api_error)
        }
        r => r.map_err(api_error),
    }
}

/// What the API says about a product.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Product {
    pub part_number: String,
    pub family: String,
    pub detail: String,
    pub status: String,
    pub specs: Vec<(String, String)>,
    /// (link key, API path), e.g. ("3-D STEP", "/v1/cad/library/…/91292A113_….STEP").
    pub links: Vec<(String, String)>,
}

impl Product {
    /// The 3-D STEP's path, else the other 3-D CAD the API offers (Parasolid, IGES).
    pub fn cad_link(&self) -> Option<&(String, String)> {
        let rank = |k: &str| {
            let k = k.to_ascii_uppercase();
            if !k.contains("3-D") && !k.contains("3D") {
                return None;
            }
            if k.contains("STEP") {
                Some(0)
            } else if k.contains("PARASOLID") || k.contains("X_T") {
                Some(1)
            } else if k.contains("IGES") || k.contains("IGS") {
                Some(2)
            } else {
                None
            }
        };
        self.links.iter().filter_map(|l| rank(&l.0).map(|r| (r, l))).min_by_key(|(r, _)| *r).map(|(_, l)| l)
    }
    /// A short description for the part's name: the family ("Socket Head Screw").
    pub fn short(&self) -> String {
        if self.family.is_empty() { self.detail.clone() } else { self.family.clone() }
    }
}

pub fn parse_product(v: &Value) -> Product {
    let specs = v["Specifications"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let vals: Vec<String> = x["Values"].as_array().map(|v| v.iter().map(s).collect()).unwrap_or_else(|| vec![s(&x["Values"])]);
                    (s(&x["Attribute"]), vals.join(", "))
                })
                .filter(|(a, _)| !a.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let links = v["Links"].as_array().map(|a| a.iter().map(|x| (s(&x["Key"]), s(&x["Value"]))).filter(|(k, v)| !k.is_empty() && !v.is_empty()).collect()).unwrap_or_default();
    Product { part_number: s(&v["PartNumber"]), family: s(&v["FamilyDescription"]), detail: s(&v["DetailDescription"]), status: s(&v["ProductStatus"]), specs, links }
}

/// Subscribe to a product (the API only describes subscribed ones) and read it.
pub fn product(set: &Settings, pn: &str) -> Result<Product, String> {
    let pn = pn.trim().to_ascii_uppercase();
    if !is_part_number(&pn) {
        return Err(format!("{pn} is not a McMaster-Carr part number (like 91292A113)"));
    }
    let url = format!("https://mcmaster.com/{pn}");
    let added = call(set, &|a, auth| a.put(&format!("{}/v1/products", api())).set("Authorization", auth).set("Content-Type", "application/json").send_string(&json!({"URL": url}).to_string()));
    // an existing subscription is fine; anything else the lookup below reports better
    let _ = added;
    let r = call(set, &|a, auth| a.get(&format!("{}/v1/products/{}", api(), encode(&pn))).set("Authorization", auth).call())?;
    let p = parse_product(&json_of(r)?);
    Ok(Product { part_number: if p.part_number.is_empty() { pn } else { p.part_number.clone() }, ..p })
}

/// The longest CAD download accepted.
const MAX_CAD: u64 = 200 << 20;

/// A part fetched for inserting: what the API said about it and its CAD file.
#[derive(Debug)]
pub struct Fetched {
    pub product: Product,
    /// File name (from the link) and bytes.
    pub file: String,
    pub bytes: Vec<u8>,
}

/// Read a product and download its 3-D CAD.
pub fn fetch(set: &Settings, pn: &str) -> Result<Fetched, String> {
    let product = product(set, pn)?;
    if product.status.eq_ignore_ascii_case("Discontinued") {
        let alt = product.links.iter().find(|l| l.0 == "SuggestedProduct").map(|l| format!(" McMaster suggests {}", l.1)).unwrap_or_default();
        return Err(format!("{} is discontinued.{alt}", product.part_number));
    }
    let (_, path) = product.cad_link().cloned().ok_or_else(|| format!("McMaster-Carr has no 3-D STEP for {}", product.part_number))?;
    let url = if path.starts_with("http") { path.clone() } else { format!("{}{}", api(), if path.starts_with('/') { path.clone() } else { format!("/{path}") }) };
    let r = call(set, &|a, auth| a.get(&url).set("Authorization", auth).call())?;
    let mut bytes = vec![];
    use std::io::Read;
    r.into_reader().take(MAX_CAD).read_to_end(&mut bytes).map_err(|e| format!("downloading the CAD: {e}"))?;
    let file = path.rsplit('/').next().unwrap_or("model.STEP").replace("%20", " ");
    Ok(Fetched { product, file, bytes })
}

pub type Reply = Result<Fetched, String>;

/// Fetch a part off the UI thread; the reply comes on the receiver.
pub fn spawn(set: Settings, pn: String, ctx: egui::Context) -> std::sync::mpsc::Receiver<Reply> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch(&set, &pn));
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_numbers() {
        for ok in ["91292A113", "6655K14", "47065T101", "1346K17", "5234K47-RD", "8975K226"] {
            assert!(is_part_number(ok), "{ok}");
        }
        for bad in ["M3", "608", "123A4", "91292a113", "91292A", "91292A1234567", "ISO 4762", "5234K47-", "A91292113"] {
            assert!(!is_part_number(bad), "{bad}");
        }
    }

    #[test]
    fn download_names() {
        assert_eq!(from_file_stem("91292A113_Black-Oxide Alloy Steel Socket Head Screw"), Some(("91292A113".into(), "Black-Oxide Alloy Steel Socket Head Screw".into())));
        assert_eq!(from_file_stem("6655K14_Ball Bearing"), Some(("6655K14".into(), "Ball Bearing".into())));
        assert_eq!(from_file_stem("6655K14"), Some(("6655K14".into(), "".into())));
        assert_eq!(from_file_stem("bracket_v2"), None);
        assert_eq!(part_name("6655K14", ""), "6655K14");
    }

    #[test]
    fn search_urls() {
        assert_eq!(search_url("91292a113"), "https://www.mcmaster.com/91292A113/");
        assert_eq!(search_url("shaft collars"), "https://www.mcmaster.com/products/shaft-collars/");
        assert_eq!(search_url(" "), "https://www.mcmaster.com/");
    }

    #[test]
    fn api_product() {
        let v = json!({
            "PartNumber": "4936K451",
            "DetailDescription": "Compact Extreme-Pressure Steel Pipe Fitting",
            "FamilyDescription": "Pipe Fitting",
            "ProductStatus": "Active",
            "Specifications": [{"Attribute": "Pipe Size", "Values": ["1/8"]}, {"Attribute": "Material", "Values": ["Steel"]}],
            "Links": [
                {"Key": "Price", "Value": "/v1/products/4936K451/price"},
                {"Key": "2-D DWG", "Value": "/v1/cad/library/20180924/4936K451.DWG"},
                {"Key": "3-D Parasolid", "Value": "/v1/cad/library/20180924/4936K451.X_T"},
                {"Key": "3-D STEP", "Value": "/v1/cad/library/20180924/4936K451_COMPACT%20EXTREME-PRESSURE%20STEEL%20PIPE%20FITTING.STEP"}
            ]
        });
        let p = parse_product(&v);
        assert_eq!(p.part_number, "4936K451");
        assert_eq!(p.specs[0], ("Pipe Size".into(), "1/8".into()));
        assert_eq!(p.cad_link().unwrap().0, "3-D STEP");
        assert_eq!(p.short(), "Pipe Fitting");
    }

    #[test]
    fn pem_identity() {
        assert!(identity_from_pem(b"nothing").is_err());
    }

    /// A client certificate as PEM and as a password-protected .pfx both make a TLS config
    /// (skipped where the openssl command is missing).
    #[test]
    fn client_certificates() {
        let dir = std::env::temp_dir().join(format!("rcad-mc-cert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (key, crt, pem, pfx) = (dir.join("k.pem"), dir.join("c.pem"), dir.join("both.pem"), dir.join("c.pfx"));
        let ok = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=rcad-test", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&crt)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("no openssl: skipped");
            return;
        }
        let both = [std::fs::read(&crt).unwrap(), std::fs::read(&key).unwrap()].concat();
        std::fs::write(&pem, both).unwrap();
        let st = std::process::Command::new("openssl").args(["pkcs12", "-export", "-passout", "pass:secret", "-inkey"]).arg(&key).arg("-in").arg(&crt).arg("-out").arg(&pfx).status().unwrap();
        assert!(st.success());
        let set = |cert: &Path, pw: &str| Settings { cert: cert.display().to_string(), cert_password: pw.into(), ..Default::default() };
        assert!(tls_config(&set(&pem, "")).is_ok());
        assert!(tls_config(&set(&pfx, "secret")).is_ok());
        assert!(tls_config(&set(&pfx, "wrong")).unwrap_err().contains("password"));
        assert!(tls_config(&set(&crt, "")).unwrap_err().contains("private key"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Log in, subscribe, read the product and download its STEP from a stand-in API server.
    #[test]
    fn fetch_from_a_test_server() {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("rcad-mc-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pem = dir.join("both.pem");
        let ok = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=rcad-test", "-keyout"])
            .arg(dir.join("k.pem"))
            .arg("-out")
            .arg(dir.join("c.pem"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("no openssl: skipped");
            return;
        }
        std::fs::write(&pem, [std::fs::read(dir.join("c.pem")).unwrap(), std::fs::read(dir.join("k.pem")).unwrap()].concat()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    match k.to_ascii_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap(),
                        "authorization" => auth = v.trim().to_string(),
                        _ => {}
                    }
                }
                let mut body = vec![0u8; len];
                std::io::Read::read_exact(&mut r, &mut body).unwrap();
                let mut it = line.split_whitespace();
                let (method, path) = (it.next().unwrap().to_string(), it.next().unwrap().to_string());
                seen.lock().unwrap().push(format!("{method} {path} {auth} {}", String::from_utf8_lossy(&body)));
                let (code, ctype, out): (u16, &str, Vec<u8>) = match (method.as_str(), path.as_str()) {
                    ("POST", "/v1/login") => (200, "application/json", br#"{"AuthToken":"tok1","ExpirationTS":"2030-01-01T00:00:00Z"}"#.to_vec()),
                    _ if auth != "Bearer tok1" => (403, "application/json", br#"{"ErrorCode":403,"ErrorMessage":"EXPIRED_AUTHORIZATION_TOKEN","ErrorDescription":"Invalid authorization token"}"#.to_vec()),
                    ("PUT", "/v1/products") => (201, "application/json", b"{}".to_vec()),
                    ("GET", "/v1/products/91292A113") => (
                        200,
                        "application/json",
                        br#"{"PartNumber":"91292A113","FamilyDescription":"Socket Head Screw","DetailDescription":"Alloy Steel Socket Head Screw, M3 x 0.5 mm Thread, 8 mm Long","ProductStatus":"Active","Specifications":[{"Attribute":"Thread Size","Values":["M3"]}],"Links":[{"Key":"3-D STEP","Value":"/v1/cad/library/20200101/91292A113_SOCKET%20HEAD%20SCREW.STEP"}]}"#.to_vec(),
                    ),
                    ("GET", p) if p.starts_with("/v1/cad/") => (200, "application/octet-stream", b"ISO-10303-21;\nEND-ISO-10303-21;\n".to_vec()),
                    _ => (404, "application/json", br#"{"ErrorCode":404,"ErrorMessage":"NOT_FOUND","ErrorDescription":"Invalid part number"}"#.to_vec()),
                };
                let mut s = stream;
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = s.write_all(&out);
            }
        });
        // SAFETY: the only test that sets these, and nothing else reads them
        unsafe {
            std::env::set_var("RCAD_MCMASTER_API", format!("http://127.0.0.1:{port}"));
        }
        let set = Settings { username: "jonas".into(), password: "pw".into(), cert: pem.display().to_string(), cert_password: String::new() };
        let f = fetch(&set, "91292a113").unwrap();
        assert_eq!(f.product.short(), "Socket Head Screw");
        assert_eq!(f.file, "91292A113_SOCKET HEAD SCREW.STEP");
        assert!(f.bytes.starts_with(b"ISO-10303-21;"));
        let log = log.lock().unwrap().clone();
        assert!(log[0].starts_with("POST /v1/login") && log[0].contains(r#""UserName":"jonas""#), "{log:?}");
        assert!(log[1].starts_with("PUT /v1/products Bearer tok1") && log[1].contains("https://mcmaster.com/91292A113"), "{log:?}");
        assert!(fetch(&set, "1234K5").unwrap_err().contains("no such part"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn warning_lists_parts() {
        assert!(share_warning(&[]).is_none());
        let w = share_warning(&["1".into(), "2".into(), "3".into(), "4".into(), "5".into()]).unwrap();
        assert!(w.contains("1, 2, 3 and 2 more"));
    }
}
