//! Digi-Key parts for PCB tabs: search and part details through Digi-Key's Product Information
//! API (v4), or with no API keys, the CSV that a digikey.com search table, cart or list downloads.
//!
//! The API needs the user's own OAuth client (a free "Production" app on developer.digikey.com
//! with the Product Information API); its client ID and secret are kept in
//! `~/.config/cad-rust-app/digikey.json` (owner-only) or come from `DIGIKEY_CLIENT_ID` and
//! `DIGIKEY_CLIENT_SECRET`. The two-legged (client credentials) token is cached until it expires.
//!
//! A Digi-Key product becomes a part: its symbol and value from the category and parameters
//! (resistance, capacitance, colour …), its footprint from the package, and the MPN,
//! manufacturer, description, datasheet and Digi-Key number for the BOM.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::pcb::Part;

const API: &str = "https://api.digikey.com";
pub const DEV_PORTAL: &str = "https://developer.digikey.com/";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// Locale site (US, UK, DE, CA …) and currency (USD, GBP, EUR …) for prices and stock.
    #[serde(default)]
    pub site: String,
    #[serde(default)]
    pub currency: String,
}

impl Settings {
    /// The credentials to use: the saved ones, else the environment's.
    pub fn creds(&self) -> Option<(String, String)> {
        let (id, secret) = (self.client_id.trim(), self.client_secret.trim());
        if !id.is_empty() && !secret.is_empty() {
            return Some((id.into(), secret.into()));
        }
        let env = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Some((env("DIGIKEY_CLIENT_ID")?, env("DIGIKEY_CLIENT_SECRET")?))
    }
    pub fn has_creds(&self) -> bool {
        self.creds().is_some()
    }
    fn site(&self) -> &str {
        if self.site.trim().is_empty() { "US" } else { self.site.trim() }
    }
    fn currency(&self) -> &str {
        if self.currency.trim().is_empty() { "USD" } else { self.currency.trim() }
    }
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("cad-rust-app").join("digikey.json"))
}

pub fn load_settings() -> Settings {
    settings_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Save the settings (with the secret) readable by the user only.
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
    *TOKEN.lock().unwrap() = None;
    Ok(p)
}

// ---------------------------------------------------------------- products

/// A Digi-Key product, from the API or a CSV row.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Product {
    /// Digi-Key part number (the cut tape one when there are several packagings).
    pub digikey: String,
    pub mpn: String,
    pub manufacturer: String,
    pub description: String,
    pub detailed: String,
    pub datasheet: String,
    pub url: String,
    pub category: String,
    /// Package / Case, and the supplier's device package.
    pub package: String,
    pub device_package: String,
    /// Technical parameters (name, value).
    pub params: Vec<(String, String)>,
    pub stock: Option<f64>,
    pub price: Option<f64>,
}

impl Product {
    pub fn param(&self, names: &[&str]) -> Option<&str> {
        names.iter().find_map(|n| self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str())).filter(|v| !v.trim().is_empty() && v.trim() != "-")
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn https(u: String) -> String {
    if u.starts_with("//") { format!("https:{u}") } else { u }
}

/// A product from the API's JSON (v4 `Product`).
pub fn parse_product(p: &Value) -> Product {
    let params: Vec<(String, String)> =
        p["Parameters"].as_array().into_iter().flatten().map(|x| (s(&x["ParameterText"]), s(&x["ValueText"]))).filter(|(k, v)| !k.is_empty() && !v.is_empty()).collect();
    let find = |n: &str| params.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|x| x.1.clone()).unwrap_or_default();
    // prefer cut tape (or the first packaging) for the Digi-Key number
    let vars: Vec<&Value> = p["ProductVariations"].as_array().map(|v| v.iter().collect()).unwrap_or_default();
    let var = vars.iter().find(|v| s(&v["PackageType"]["Name"]).to_ascii_lowercase().contains("cut tape")).or(vars.first());
    let mut digikey = var.map(|v| s(&v["DigiKeyProductNumber"])).unwrap_or_default();
    if digikey.is_empty() {
        digikey = s(&p["DigiKeyProductNumber"]);
    }
    let mut category = s(&p["Category"]["Name"]);
    // the deepest child category says the most (Chip Resistor - Surface Mount)
    let mut c = &p["Category"]["ChildCategories"];
    while let Some(first) = c.as_array().and_then(|a| a.first()) {
        let n = s(&first["Name"]);
        if !n.is_empty() {
            category = n;
        }
        c = &first["ChildCategories"];
    }
    Product {
        digikey,
        mpn: s(&p["ManufacturerProductNumber"]),
        manufacturer: s(&p["Manufacturer"]["Name"]),
        description: s(&p["Description"]["ProductDescription"]),
        detailed: s(&p["Description"]["DetailedDescription"]),
        datasheet: https(s(&p["DatasheetUrl"])),
        url: https(s(&p["ProductUrl"])),
        category,
        package: find("Package / Case"),
        device_package: find("Supplier Device Package"),
        stock: p["QuantityAvailable"].as_f64(),
        price: p["UnitPrice"].as_f64().filter(|v| *v > 0.0),
        params,
    }
}

// ---------------------------------------------------------------- the API

static TOKEN: Mutex<Option<(String, String, Instant)>> = Mutex::new(None);

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(40)).try_proxy_from_env(true).build()
}

fn api_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let msg = [&v["detail"], &v["ErrorMessage"], &v["error_description"], &v["title"], &v["message"]]
                .iter()
                .map(|x| s(x))
                .find(|x| !x.is_empty())
                .unwrap_or_else(|| body.chars().take(200).collect());
            match code {
                401 => format!("Digi-Key refused the client ID or secret (401): {msg}"),
                403 => format!("Digi-Key refused the request (403): {msg}. Check that the app has the Product Information API"),
                404 => "Digi-Key has no such part (404)".into(),
                429 => "Digi-Key's rate limit was reached (429); try again in a minute".into(),
                _ => format!("Digi-Key answered {code}: {msg}"),
            }
        }
        ureq::Error::Transport(t) => format!("couldn't reach Digi-Key: {t}"),
    }
}

fn json_of(r: ureq::Response) -> Result<Value, String> {
    let t = r.into_string().map_err(|e| format!("Digi-Key's answer: {e}"))?;
    serde_json::from_str(&t).map_err(|e| format!("Digi-Key's answer is not JSON: {e}"))
}

fn token(set: &Settings) -> Result<(String, String), String> {
    let (id, secret) = set.creds().ok_or("no Digi-Key client ID and secret: add them under Digi-Key settings (or set DIGIKEY_CLIENT_ID and DIGIKEY_CLIENT_SECRET)")?;
    if let Some((tid, tok, until)) = TOKEN.lock().unwrap().clone() {
        if tid == id && Instant::now() < until {
            return Ok((id, tok));
        }
    }
    let r = http().post(&format!("{API}/v1/oauth2/token")).send_form(&[("client_id", &id), ("client_secret", &secret), ("grant_type", "client_credentials")]).map_err(api_error)?;
    let v = json_of(r)?;
    let tok = s(&v["access_token"]);
    if tok.is_empty() {
        return Err("Digi-Key gave no access token".into());
    }
    let life = v["expires_in"].as_f64().unwrap_or(600.0);
    *TOKEN.lock().unwrap() = Some((id.clone(), tok.clone(), Instant::now() + Duration::from_secs_f64((life - 30.0).max(30.0))));
    Ok((id, tok))
}

fn request(set: &Settings, method: &str, path: &str) -> Result<ureq::Request, String> {
    let (id, tok) = token(set)?;
    Ok(http()
        .request(method, &format!("{API}{path}"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("X-DIGIKEY-Client-Id", &id)
        .set("X-DIGIKEY-Locale-Site", set.site())
        .set("X-DIGIKEY-Locale-Language", "en")
        .set("X-DIGIKEY-Locale-Currency", set.currency())
        .set("Accept", "application/json"))
}

/// Keyword search (a part number, or words like "10k 0805 1%").
pub fn search(set: &Settings, keywords: &str, limit: usize) -> Result<Vec<Product>, String> {
    let body = json!({"Keywords": keywords.trim(), "Limit": limit.clamp(1, 50), "Offset": 0});
    let v = json_of(request(set, "POST", "/products/v4/search/keyword")?.set("Content-Type", "application/json").send_string(&body.to_string()).map_err(api_error)?)?;
    let mut out: Vec<Product> = v["ExactMatches"].as_array().into_iter().flatten().map(parse_product).collect();
    for p in v["Products"].as_array().into_iter().flatten().map(parse_product) {
        if !out.iter().any(|o| o.mpn == p.mpn && o.manufacturer == p.manufacturer) {
            out.push(p);
        }
    }
    Ok(out)
}

/// One part's details, by Digi-Key or manufacturer part number.
pub fn details(set: &Settings, part_number: &str) -> Result<Product, String> {
    let pn = part_number_of(part_number);
    let v = request(set, "GET", &format!("/products/v4/search/{}/productdetails", encode(&pn)))?.call().map_err(api_error)?;
    let v = json_of(v)?;
    Ok(parse_product(&v["Product"]))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A part number from what was pasted: the number itself, or a digikey.com product link
/// (`…/products/detail/yageo/RC0805FR-0710KL/727618` gives the MPN).
pub fn part_number_of(text: &str) -> String {
    let t = text.trim();
    if let Some(k) = t.find("/products/detail/") {
        let segs: Vec<&str> = t[k + 17..].split(['/', '?', '#']).collect();
        if let Some(m) = segs.get(1).filter(|m| !m.is_empty()) {
            return m.replace("%2F", "/").replace("%2f", "/");
        }
    }
    t.to_string()
}

/// The digikey.com search page for some words, for searching without API keys.
pub fn web_search_url(q: &str) -> String {
    format!("https://www.digikey.com/en/products/result?keywords={}", encode(q.trim()))
}

/// The product page for a part (its link when known, else a search for the number).
pub fn product_url(part: &Part) -> String {
    let pn = if part.digikey.is_empty() { &part.mpn } else { &part.digikey };
    web_search_url(pn)
}

// ---------------------------------------------------------------- CSV (no API keys)

fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = vec![];
    let mut row = vec![];
    let mut f = String::new();
    let mut q = false;
    let mut it = text.trim_start_matches('\u{feff}').chars().peekable();
    // Digi-Key's table downloads are comma separated; some lists use tabs or semicolons
    let first = text.lines().next().unwrap_or("");
    let sep = [',', '\t', ';'].into_iter().max_by_key(|c| first.matches(*c).count()).unwrap_or(',');
    while let Some(c) = it.next() {
        match c {
            '"' if q && it.peek() == Some(&'"') => {
                f.push('"');
                it.next();
            }
            '"' => q = !q,
            c if c == sep && !q => row.push(std::mem::take(&mut f)),
            '\r' if !q => {}
            '\n' if !q => {
                row.push(std::mem::take(&mut f));
                if row.iter().any(|x| !x.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                }
                row.clear();
            }
            c => f.push(c),
        }
    }
    row.push(f);
    if row.iter().any(|x| !x.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

fn norm(h: &str) -> String {
    h.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '#').collect::<String>().to_ascii_lowercase()
}

/// Products from a CSV downloaded from digikey.com: a search results table ("Download Table"),
/// a cart, a myList or a BOM Manager export. Unknown columns become parameters.
pub fn parse_csv(text: &str) -> Result<Vec<Product>, String> {
    let rows = csv_rows(text);
    // the header is the first row naming a part number column
    let is_pn = |h: &str| {
        matches!(
            norm(h).as_str(),
            "digikeypart#"
                | "digikeypartnumber"
                | "dkpart#"
                | "digikeyproductnumber"
                | "mfrpart#"
                | "manufacturerpartnumber"
                | "manufacturerproductnumber"
                | "partnumber"
                | "mfrpartnumber"
        )
    };
    let hi = rows.iter().position(|r| r.iter().any(|h| is_pn(h))).ok_or("this doesn't look like a Digi-Key CSV (no part number column)")?;
    let head: Vec<String> = rows[hi].iter().map(|h| norm(h)).collect();
    let col = |names: &[&str]| names.iter().find_map(|n| head.iter().position(|h| h == n));
    let c_dk = col(&["digikeypart#", "digikeypartnumber", "dkpart#", "digikeyproductnumber", "partnumber"]);
    let c_mpn = col(&["mfrpart#", "manufacturerpartnumber", "manufacturerproductnumber", "mfrpartnumber"]);
    let c_mfr = col(&["mfr", "manufacturer", "manufacturername"]);
    let c_desc = col(&["description", "productdescription"]);
    let c_det = col(&["detaileddescription"]);
    let c_ds = col(&["datasheet", "datasheets", "datasheeturl"]);
    let c_url = col(&["producturl", "productlink", "url"]);
    let c_stock = col(&["stock", "quantityavailable", "available"]);
    let c_price = col(&["price", "unitprice"]);
    let c_cat = col(&["category", "productcategory"]);
    let used: Vec<usize> = [c_dk, c_mpn, c_mfr, c_desc, c_det, c_ds, c_url, c_stock, c_price, c_cat].into_iter().flatten().collect();
    let skip = [
        "image",
        "quantity",
        "qty",
        "minqty",
        "minimumquantity",
        "qty",
        "index",
        "#",
        "packaging",
        "productstatus",
        "supplier",
        "series",
        "customerreference",
        "backorder",
        "extendedprice",
        "unitpriceusd",
        "package",
    ];
    let mut out = vec![];
    for r in &rows[hi + 1..] {
        let get = |c: Option<usize>| c.and_then(|c| r.get(c)).map(|x| x.trim().to_string()).unwrap_or_default();
        let num = |c: Option<usize>| {
            let t: String = get(c).chars().filter(|ch| ch.is_ascii_digit() || *ch == '.').collect();
            t.parse::<f64>().ok()
        };
        let params: Vec<(String, String)> = rows[hi]
            .iter()
            .enumerate()
            .filter(|(k, h)| !used.contains(k) && !skip.contains(&norm(h).as_str()) && !h.trim().is_empty())
            .filter_map(|(k, h)| r.get(k).map(|v| (h.trim().to_string(), v.trim().to_string())))
            .filter(|(_, v)| !v.is_empty() && v != "-")
            .collect();
        let find = |n: &str| params.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|x| x.1.clone()).unwrap_or_default();
        let mut p = Product {
            digikey: get(c_dk),
            mpn: get(c_mpn),
            manufacturer: get(c_mfr),
            description: get(c_desc),
            detailed: get(c_det),
            datasheet: https(get(c_ds)),
            url: https(get(c_url)),
            category: get(c_cat),
            package: find("Package / Case"),
            device_package: find("Supplier Device Package"),
            stock: num(c_stock),
            price: num(c_price),
            params,
        };
        // a "Datasheet" cell can hold several links; keep the first
        if let Some(first) = p.datasheet.split_whitespace().next() {
            p.datasheet = first.to_string();
        }
        if p.mpn.is_empty() && p.digikey.is_empty() {
            continue;
        }
        out.push(p);
    }
    if out.is_empty() {
        return Err("no parts in this CSV".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- products as parts

/// The library symbol a product is drawn with.
pub fn symbol_for(p: &Product) -> &'static str {
    // whole words of the category and descriptions: "LED" must not match "CONTROLLED"
    let d: String =
        format!("{} {} {}", p.category, p.description, p.detailed).to_ascii_uppercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { ' ' }).collect();
    let words: Vec<&str> = d.split_whitespace().collect();
    let padded = format!(" {} ", words.join(" "));
    // a key matches a whole word (or phrase); keys of 5 letters or more also match a longer word (RESISTORS)
    let has =
        |keys: &[&str]| keys.iter().any(|k| if k.contains(' ') { padded.contains(&format!(" {k} ")) } else { words.iter().any(|w| w == k || (k.len() >= 5 && w.starts_with(k))) });
    if has(&["POTENTIOMETER", "TRIMMER", "POT"]) {
        "RV"
    } else if has(&["RES", "RESISTOR", "THERMISTOR"]) {
        "R"
    } else if has(&["CAP ALUM", "ALUMINUM ELECTROLYTIC", "ALUMINUM - POLYMER", "CAP TANT", "TANTALUM"]) {
        "CP"
    } else if has(&["CAP", "CAPACITOR"]) {
        "C"
    } else if has(&["FIXED IND", "INDUCTOR", "FERRITE"]) {
        "L"
    } else if has(&["LED", "LIGHT EMITTING"]) && !has(&["DRIVER"]) {
        "LED"
    } else if has(&["MOSFET", "FET", "FETS"]) {
        if has(&["P-CH", "P-CHANNEL", "PCH"]) { "Q_PMOS" } else { "Q_NMOS" }
    } else if has(&["TRANS PNP", "PNP"]) {
        "Q_PNP"
    } else if has(&["TRANS NPN", "NPN", "BIPOLAR"]) {
        "Q_NPN"
    } else if has(&["DIODE", "ZENER", "SCHOTTKY", "RECTIFIER", "TVS"]) {
        "D"
    } else if has(&["CRYSTAL", "RESONATOR", "XTAL"]) {
        "Y"
    } else if has(&["SWITCH TACTILE", "TACTILE", "PUSHBUTTON", "SWITCH PUSH"]) {
        "SW"
    } else if has(&["CONN", "HEADER", "TERMINAL BLOCK", "CONNECTOR"]) {
        "J"
    } else if has(&["SERVO"]) {
        "SERVO"
    } else if has(&["STEPPER"]) {
        "STEPPER"
    } else if has(&["MOTOR"]) {
        "M"
    } else {
        "U"
    }
}

/// "10 kOhms" → 10k, "0.1 µF" → 100n, "4.7 µH" → 4.7u, "0.1U" → 100n.
fn eng(text: &str) -> Option<String> {
    let t = text.trim().replace(['µ', 'μ', 'U'], "u");
    let v = crate::circuit::parse_si_lenient(&t)?;
    if !(v.is_finite() && v > 0.0) {
        return None;
    }
    Some(crate::circuit::si(v, "").replace(' ', "").replace('µ', "u"))
}

/// A value from a Digi-Key style description: "RES 10K OHM 1% 1/8W 0805" (key OHM),
/// "CAP CER 0.1UF 50V X7R 0603" (key F), "FIXED IND 10UH 1A" (key H).
fn value_in_description(desc: &str, key: &str) -> Option<String> {
    let d = desc.to_ascii_uppercase();
    let w: Vec<&str> = d.split_whitespace().collect();
    for (k, x) in w.iter().enumerate() {
        if key == "OHM" {
            if x.starts_with("OHM") && k > 0 {
                return eng(w[k - 1]);
            }
        } else if x.len() > key.len() && x.ends_with(key) && x.starts_with(|c: char| c.is_ascii_digit()) {
            return eng(&x[..x.len() - key.len()]);
        }
    }
    None
}

/// The value a product gets on the schematic (and in the simulation).
pub fn value_for(p: &Product, symbol: &str) -> String {
    let by = |names: &[&str]| p.param(names).and_then(eng);
    let desc = |key: &str| value_in_description(&p.description, key);
    let v = match symbol {
        "R" | "RV" => by(&["Resistance", "Resistance (Ohms)"]).or_else(|| desc("OHM")),
        "C" | "CP" => by(&["Capacitance"]).or_else(|| desc("F")),
        "L" => by(&["Inductance"]).or_else(|| desc("H")),
        "Y" => p.param(&["Frequency"]).map(|f| f.replace(' ', "")),
        "LED" => p.param(&["Color", "Colour"]).map(|c| c.split([',', '/']).next().unwrap_or(c).trim().to_string()).or_else(|| {
            // "LED RED CLEAR 0805 SMD"
            let d = p.description.to_ascii_uppercase();
            ["RED", "GREEN", "BLUE", "YELLOW", "AMBER", "ORANGE", "WHITE", "INFRARED", "IR", "UV", "VIOLET", "RGB"].iter().find(|c| d.split_whitespace().any(|w| w == **c)).map(
                |c| {
                    let mut s = c.to_ascii_lowercase();
                    s[..1].make_ascii_uppercase();
                    s
                },
            )
        }),
        _ => None,
    };
    v.unwrap_or_else(|| if p.mpn.is_empty() { p.digikey.clone() } else { p.mpn.clone() })
}

/// The library footprint for a product's package, for this symbol (None: no close match).
pub fn footprint_for(p: &Product, symbol: &str) -> Option<String> {
    let text = format!("{} | {} | {}", p.package, p.device_package, p.description).to_ascii_uppercase().replace(['(', ')', ','], " ");
    let has = |k: &str| text.contains(k);
    let lib = crate::pcb_lib::footprint_names();
    let ok = |f: String| lib.iter().any(|x| *x == f).then_some(f);
    // chips: imperial size codes
    // imperial size words, not the metric ones: "0201 (0603 Metric)" is not an 0603
    let toks: Vec<&str> = text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let imperial: Vec<&str> = toks.iter().enumerate().filter(|(k, _)| toks.get(k + 1) != Some(&"METRIC")).map(|x| *x.1).collect();
    for size in ["0402", "0603", "0805", "1206"] {
        if imperial.contains(&size) {
            let prefix = match symbol {
                "R" => "R",
                "C" | "CP" => "C",
                "L" => "L",
                "LED" => "LED",
                _ => "",
            };
            if !prefix.is_empty() {
                if let Some(f) = ok(format!("{prefix}_{size}")) {
                    return Some(f);
                }
            }
        }
    }
    // pin count before a package family: "8-SOIC", "14-TSSOP", "8-DIP"
    let counted = |fam: &[&str]| -> Option<usize> {
        text.split(|c: char| c.is_whitespace() || c == '|').find_map(|w| {
            let (n, f) = w.split_once('-')?;
            (fam.iter().any(|x| f.starts_with(x))).then(|| n.parse::<usize>().ok()).flatten()
        })
    };
    if let Some(n) = counted(&["SOIC", "SO"]) {
        return ok(format!("SOIC-{n}"));
    }
    if let Some(n) = counted(&["TSSOP", "MSOP"]) {
        return ok(format!("TSSOP-{n}"));
    }
    if let Some(n) = counted(&["DIP", "PDIP"]) {
        return ok(format!("DIP-{n}"));
    }
    if has("SOT-23-5") || has("SOT-753") || has("SC-74A") || has("SOT-25") {
        return ok("SOT-23-5".into());
    }
    if has("SOT-223") || has("TO-261") {
        return ok("SOT-223".into());
    }
    if has("SOT-23") || has("TO-236") {
        return ok("SOT-23".into());
    }
    if has("SOD-123") {
        return ok("D_SOD-123".into());
    }
    if has("DO-41") || has("DO-204AL") {
        return ok("D_DO-41".into());
    }
    if has("TO-92") || has("TO-226") {
        return ok("TO-92".into());
    }
    if has("TO-220") {
        return ok("TO-220".into());
    }
    if has("HC-49") || has("HC49") {
        return ok("Crystal_HC49".into());
    }
    if symbol == "LED" && (has("T-1 3/4") || has("5MM")) {
        return ok("LED_D5".into());
    }
    if symbol == "CP" && has("RADIAL") {
        // the can diameter: "Radial, Can" with "Size / Dimension 0.315\" Dia (8.00mm)"
        let dia = p.param(&["Size / Dimension"]).and_then(|s| {
            let k = s.find("mm")?;
            let n: String = s[..k].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
            n.parse::<f64>().ok()
        });
        return ok(if dia.is_some_and(|d| d > 6.5) { "CP_D8".into() } else { "CP_D5".into() });
    }
    if symbol == "R" && has("AXIAL") {
        return ok("R_Axial".into());
    }
    if symbol == "J" {
        // "Header, 1x04", positions
        let pos = p.param(&["Number of Positions", "Number of Positions Loaded"]).and_then(|x| x.split_whitespace().next()?.parse::<usize>().ok());
        let rows = p.param(&["Number of Rows"]).and_then(|x| x.split_whitespace().next()?.parse::<usize>().ok()).unwrap_or(1);
        if let Some(n) = pos {
            if has("TERMINAL") {
                return ok(format!("Terminal_{n}P"));
            }
            return ok(if rows == 2 { format!("PinHeader_2x{:02}", n / 2) } else { format!("PinHeader_1x{n:02}") });
        }
    }
    None
}

/// Fields worth keeping in the BOM from a product's parameters.
const BOM_PARAMS: &[&str] =
    &["Tolerance", "Power (Watts)", "Voltage - Rated", "Temperature Coefficient", "Current Rating (Amps)", "Voltage - Supply", "Current - Output", "Voltage - Output (Min/Fixed)"];

/// Fill a part from a product: MPN, manufacturer, description, datasheet, Digi-Key number,
/// package and BOM fields, and (when `retype`) its value and footprint. Returns what changed.
pub fn apply(part: &mut Part, p: &Product, retype: bool) -> Vec<String> {
    let mut changed = vec![];
    // an LCSC number in the MPN box moves to an LCSC field, so the JLCPCB column keeps it
    let m = part.mpn.trim().to_string();
    if m.len() > 1 && m.starts_with('C') && m[1..].chars().all(|c| c.is_ascii_digit()) && m != p.mpn && !part.fields.iter().any(|(k, _)| k.eq_ignore_ascii_case("LCSC")) {
        part.fields.push(("LCSC".into(), m));
        changed.push("LCSC".to_string());
    }
    let mut set = |name: &str, field: &mut String, v: &str| {
        if !v.is_empty() && field != v {
            *field = v.to_string();
            changed.push(name.to_string());
        }
    };
    set("mpn", &mut part.mpn, &p.mpn);
    set("manufacturer", &mut part.manufacturer, &p.manufacturer);
    set("description", &mut part.description, if p.description.is_empty() { &p.detailed } else { &p.description });
    set("datasheet", &mut part.datasheet, &p.datasheet);
    set("digikey", &mut part.digikey, &p.digikey);
    let pkg = if p.package.is_empty() { &p.device_package } else { &p.package };
    set("package", &mut part.package, pkg);
    if retype {
        let v = value_for(p, &part.symbol);
        set("value", &mut part.value, &v);
        if let Some(f) = footprint_for(p, &part.symbol) {
            set("footprint", &mut part.footprint, &f);
        }
    }
    for name in BOM_PARAMS {
        if let Some(v) = p.param(&[name]) {
            let key = name.split(" (").next().unwrap_or(name).to_string();
            match part.fields.iter_mut().find(|(k, _)| *k == key) {
                Some(f) if f.1 != v => {
                    f.1 = v.to_string();
                    changed.push(key);
                }
                Some(_) => {}
                None => {
                    part.fields.push((key.clone(), v.to_string()));
                    changed.push(key);
                }
            }
        }
    }
    changed
}

/// What the agent and the UI show of a product.
pub fn product_json(p: &Product) -> Value {
    let sym = symbol_for(p);
    json!({
        "digikey": p.digikey, "mpn": p.mpn, "manufacturer": p.manufacturer, "description": p.description,
        "detailed": p.detailed, "datasheet": p.datasheet, "category": p.category, "package": p.package,
        "stock": p.stock, "price": p.price, "symbol": sym, "value": value_for(p, sym), "footprint": footprint_for(p, sym),
        "parameters": p.params.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------- background calls for the UI

pub enum Job {
    Search(String),
    Details(String),
}

pub type Reply = Result<Vec<Product>, String>;

/// Run a Digi-Key call off the UI thread; the reply comes on the receiver.
pub fn spawn(set: Settings, job: Job, ctx: egui::Context) -> std::sync::mpsc::Receiver<Reply> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = match job {
            Job::Search(q) => search(&set, &q, 20),
            Job::Details(pn) => details(&set, &pn).map(|p| vec![p]),
        };
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resistor() -> Value {
        json!({
            "Description": {"ProductDescription": "RES 10K OHM 1% 1/8W 0805", "DetailedDescription": "10 kOhms ±1% 0.125W, 1/8W Chip Resistor 0805 (2012 Metric) Moisture Resistant Thick Film"},
            "Manufacturer": {"Id": 13, "Name": "YAGEO"},
            "ManufacturerProductNumber": "RC0805FR-0710KL",
            "UnitPrice": 0.1, "ProductUrl": "https://www.digikey.com/en/products/detail/yageo/RC0805FR-0710KL/727618",
            "DatasheetUrl": "//www.yageo.com/upload/media/product/productsearch/datasheet/rchip/PYu-RC_Group_51_RoHS_L_12.pdf",
            "QuantityAvailable": 4000000,
            "ProductVariations": [
                {"DigiKeyProductNumber": "311-10.0KCRTR-ND", "PackageType": {"Id": 1, "Name": "Tape & Reel (TR)"}},
                {"DigiKeyProductNumber": "311-10.0KCRCT-ND", "PackageType": {"Id": 2, "Name": "Cut Tape (CT)"}}
            ],
            "Parameters": [
                {"ParameterId": 2085, "ParameterText": "Resistance", "ValueText": "10 kOhms"},
                {"ParameterText": "Tolerance", "ValueText": "±1%"},
                {"ParameterText": "Power (Watts)", "ValueText": "0.125W, 1/8W"},
                {"ParameterText": "Package / Case", "ValueText": "0805 (2012 Metric)"},
                {"ParameterText": "Supplier Device Package", "ValueText": "0805"}
            ],
            "Category": {"Name": "Resistors", "ChildCategories": [{"Name": "Chip Resistor - Surface Mount", "ChildCategories": []}]}
        })
    }

    #[test]
    fn api_product_becomes_a_resistor() {
        let p = parse_product(&resistor());
        assert_eq!(p.digikey, "311-10.0KCRCT-ND");
        assert_eq!(p.category, "Chip Resistor - Surface Mount");
        assert!(p.datasheet.starts_with("https://www.yageo.com/"));
        assert_eq!(symbol_for(&p), "R");
        assert_eq!(value_for(&p, "R"), "10k");
        assert_eq!(footprint_for(&p, "R").as_deref(), Some("R_0805"));
        let mut part = Part { symbol: "R".into(), value: "1k".into(), footprint: "R_0603".into(), ..Default::default() };
        let ch = apply(&mut part, &p, true);
        assert_eq!((part.value.as_str(), part.footprint.as_str(), part.mpn.as_str(), part.manufacturer.as_str()), ("10k", "R_0805", "RC0805FR-0710KL", "YAGEO"));
        assert!(ch.contains(&"datasheet".to_string()));
        assert_eq!(part.fields.iter().find(|f| f.0 == "Tolerance").map(|f| f.1.as_str()), Some("±1%"));
        // applying again changes nothing; an LCSC number in the MPN box is kept as a field
        let mut lc = Part { symbol: "R".into(), mpn: "C17414".into(), ..Default::default() };
        apply(&mut lc, &p, true);
        assert_eq!((lc.mpn.as_str(), lc.fields[0].0.as_str(), lc.fields[0].1.as_str()), ("RC0805FR-0710KL", "LCSC", "C17414"));
        assert!(apply(&mut part, &p, true).is_empty());
    }

    #[test]
    fn values_and_packages_of_other_kinds() {
        let mk = |desc: &str, params: &[(&str, &str)]| Product {
            description: desc.into(),
            mpn: "X1".into(),
            params: params.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            package: params.iter().find(|x| x.0 == "Package / Case").map(|x| x.1.to_string()).unwrap_or_default(),
            ..Default::default()
        };
        let c = mk("CAP CER 0.1UF 50V X7R 0603", &[("Capacitance", "0.1 µF"), ("Package / Case", "0603 (1608 Metric)")]);
        assert_eq!((symbol_for(&c), value_for(&c, "C").as_str(), footprint_for(&c, "C").as_deref()), ("C", "100n", Some("C_0603")));
        let op = mk("IC OPAMP GP 2 CIRCUIT 8SOIC", &[("Package / Case", "8-SOIC (0.154\", 3.90mm Width)")]);
        assert_eq!((symbol_for(&op), value_for(&op, "U").as_str(), footprint_for(&op, "U").as_deref()), ("U", "X1", Some("SOIC-8")));
        let led = mk("LED RED CLEAR 0805 SMD", &[("Package / Case", "0805 (2012 Metric)")]);
        assert_eq!((symbol_for(&led), value_for(&led, "LED").as_str(), footprint_for(&led, "LED").as_deref()), ("LED", "Red", Some("LED_0805")));
        let q = mk("TRANS NPN 40V 0.2A SOT23-3", &[("Package / Case", "TO-236-3, SC-59, SOT-23-3")]);
        assert_eq!((symbol_for(&q), footprint_for(&q, "Q_NPN").as_deref()), ("Q_NPN", Some("SOT-23")));
        let fet = mk("MOSFET P-CH 30V 4.2A SOT23", &[("Package / Case", "TO-236-3, SC-59, SOT-23-3")]);
        assert_eq!(symbol_for(&fet), "Q_PMOS");
        let reg = mk("IC REG LINEAR 3.3V 1A SOT223-3", &[("Package / Case", "TO-261-4, TO-261AA")]);
        assert_eq!(footprint_for(&reg, "U").as_deref(), Some("SOT-223"));
        let tiny = mk("RES 1K OHM 1% 1/20W 0201", &[("Package / Case", "0201 (0603 Metric)")]);
        assert_eq!(footprint_for(&tiny, "R"), None);
        let drv = mk("IC LED DRIVER LINEAR 8SOIC", &[("Package / Case", "8-SOIC")]);
        assert_eq!(symbol_for(&drv), "U");
        let iso = mk("DGTL ISOLATOR 2500VRMS 2CH 8SOIC", &[]);
        assert_eq!(symbol_for(&iso), "U");
        // a value from the description when the parameter is missing
        let r = mk("RES 4.7K OHM 5% 1/4W 1206", &[]);
        assert_eq!(value_for(&r, "R"), "4.7k");
        let l = mk("FIXED IND 10UH 1A 200 MOHM SMD", &[("Inductance", "10 µH")]);
        assert_eq!((symbol_for(&l), value_for(&l, "L").as_str()), ("L", "10u"));
    }

    #[test]
    fn csv_from_a_digikey_search_table() {
        let csv = "\u{feff}Datasheet,Image,DK Part #,Mfr Part #,Mfr,Supplier,Description,Stock,Price,@ qty,Min Qty,Package,Series,Product Status,Resistance,Tolerance,Power (Watts),Package / Case,Supplier Device Package\n\
            https://www.yageo.com/rc.pdf,//img,311-10.0KCRCT-ND,RC0805FR-0710KL,YAGEO,YAGEO,\"RES 10K OHM 1% 1/8W 0805\",\"4,005,123\",$0.10,1,1,Cut Tape (CT),RC,Active,10 kOhms,±1%,\"0.125W, 1/8W\",0805 (2012 Metric),0805\n";
        let v = parse_csv(csv).unwrap();
        assert_eq!(v.len(), 1);
        let p = &v[0];
        assert_eq!((p.digikey.as_str(), p.mpn.as_str(), p.manufacturer.as_str()), ("311-10.0KCRCT-ND", "RC0805FR-0710KL", "YAGEO"));
        assert_eq!(p.stock, Some(4005123.0));
        assert_eq!(p.price, Some(0.10));
        assert_eq!(p.datasheet, "https://www.yageo.com/rc.pdf");
        assert_eq!((value_for(p, "R").as_str(), footprint_for(p, "R").as_deref()), ("10k", Some("R_0805")));
        // a cart export
        let cart = "Index,Quantity,Part Number,Manufacturer Part Number,Description,Customer Reference,Available,Backorder,Unit Price,Extended Price\n1,10,296-1395-5-ND,NE555P,IC OSC SINGLE TIMER 100KHZ 8-DIP,,1000,0,0.46,4.60\n";
        let v = parse_csv(cart).unwrap();
        assert_eq!((v[0].digikey.as_str(), v[0].mpn.as_str()), ("296-1395-5-ND", "NE555P"));
        assert_eq!(footprint_for(&v[0], "U").as_deref(), Some("DIP-8"));
        assert!(parse_csv("a,b\n1,2\n").is_err());
    }

    #[test]
    fn part_numbers_from_links() {
        assert_eq!(part_number_of("https://www.digikey.com/en/products/detail/yageo/RC0805FR-0710KL/727618"), "RC0805FR-0710KL");
        assert_eq!(part_number_of(" 311-10.0KCRCT-ND "), "311-10.0KCRCT-ND");
        assert_eq!(encode("311-10.0KCRCT-ND"), "311-10.0KCRCT-ND");
        assert_eq!(encode("A/B #1"), "A%2FB%20%231");
    }
}
