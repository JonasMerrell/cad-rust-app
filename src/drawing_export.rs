//! Drawing exporters: PDF (hand-written, vector), DXF (R12 ASCII, with layers, line types,
//! arcs and dimensions) and SVG.
//! All take sheet primitives in millimetres with the origin at the bottom left.

use crate::drawing_ui::Prim;

fn esc_pdf(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            c if c.is_ascii() => c.to_string(),
            // WinAnsiEncoding covers the Latin-1 range (Ø, °, ±, ×, …)
            c if (c as u32) >= 0xa0 && (c as u32) <= 0xff => format!("\\{:03o}", c as u32),
            _ => "?".into(),
        })
        .collect()
}

pub fn pdf(prims: &[Prim], size: [f64; 2]) -> Vec<u8> {
    pdf_pages(&[prims], size)
}

/// A PDF with one page per sheet.
pub fn pdf_pages(pages: &[&[Prim]], size: [f64; 2]) -> Vec<u8> {
    let k = 72.0 / 25.4;
    let contents: Vec<String> = pages.iter().map(|p| page_stream(p)).collect();
    let n = contents.len().max(1);
    // objects: 1 catalog, 2 pages, 3 font, then (page, contents) pairs
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    for (i, c) in contents.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", size[0] * k, size[1] * k, 5 + 2 * i));
        objs.push(format!("<< /Length {} >>\nstream\n{}endstream", c.len(), c));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", objs.len() + 1, xref).as_bytes());
    out
}

fn page_stream(prims: &[Prim]) -> String {
    let k = 72.0 / 25.4;
    let mut c = String::new();
    c.push_str("1 J 1 j\n");
    for p in prims {
        match p {
            Prim::Line { a, b, w, dashed, color } => {
                c.push_str(&format!(
                    "{:.3} {:.3} {:.3} RG {:.3} w {} {:.3} {:.3} m {:.3} {:.3} l S\n",
                    color[0] as f64 / 255.0,
                    color[1] as f64 / 255.0,
                    color[2] as f64 / 255.0,
                    w * k,
                    if *dashed { "[3 2] 0 d" } else { "[] 0 d" },
                    a[0] * k,
                    a[1] * k,
                    b[0] * k,
                    b[1] * k
                ));
            }
            Prim::Arc { c: ctr, r, a0, a1, w, dashed, color } => {
                let pts = Prim::arc_points(*ctr, *r, *a0, *a1);
                c.push_str(&format!("{:.3} {:.3} {:.3} RG {:.3} w {} ", color[0] as f64 / 255.0, color[1] as f64 / 255.0, color[2] as f64 / 255.0, w * k, if *dashed { "[3 2] 0 d" } else { "[] 0 d" }));
                for (i, q) in pts.iter().enumerate() {
                    c.push_str(&format!("{:.3} {:.3} {} ", q[0] * k, q[1] * k, if i == 0 { "m" } else { "l" }));
                }
                c.push_str("S\n");
            }
            Prim::Chain { a, b, w, color } => {
                let [l, g, dot, g2] = Prim::CHAIN.map(|v| v * k);
                c.push_str(&format!(
                    "{:.3} {:.3} {:.3} RG {:.3} w [{l:.2} {g:.2} {dot:.2} {g2:.2}] 0 d {:.3} {:.3} m {:.3} {:.3} l S\n",
                    color[0] as f64 / 255.0,
                    color[1] as f64 / 255.0,
                    color[2] as f64 / 255.0,
                    w * k,
                    a[0] * k,
                    a[1] * k,
                    b[0] * k,
                    b[1] * k
                ));
            }
            Prim::Dim(_) | Prim::End => {}
            Prim::Text { at, size, text, anchor, color } => {
                let pt = size * k;
                // Helvetica averages ~0.5 em per character
                let wdt = text.chars().count() as f64 * pt * 0.5;
                let x = at[0] * k
                    - match anchor {
                        1 => wdt / 2.0,
                        2 => wdt,
                        _ => 0.0,
                    };
                c.push_str(&format!(
                    "{:.3} {:.3} {:.3} rg BT /F1 {:.2} Tf {:.3} {:.3} Td ({}) Tj ET\n",
                    color[0] as f64 / 255.0,
                    color[1] as f64 / 255.0,
                    color[2] as f64 / 255.0,
                    pt,
                    x,
                    at[1] * k,
                    esc_pdf(text)
                ));
            }
        }
    }
    c
}

/// Text for DXF: Ø, ° and ± as %%c, %%d and %%p, anything else outside ASCII as \U+XXXX.
fn esc_dxf(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'Ø' | 'ø' | '⌀' => "%%c".to_string(),
            '°' => "%%d".to_string(),
            '±' => "%%p".to_string(),
            '−' => "-".to_string(),
            c if c.is_ascii() => c.to_string(),
            c => format!("\\U+{:04X}", c as u32),
        })
        .collect()
}

/// The layer a primitive goes on (each layer has its line type in the TABLES section).
fn dxf_layer(p: &Prim) -> &'static str {
    match p {
        Prim::Line { dashed: true, .. } | Prim::Arc { dashed: true, .. } => "HIDDEN",
        Prim::Chain { .. } => "CENTER",
        Prim::Text { .. } => "TEXT",
        Prim::Line { color, .. } | Prim::Arc { color, .. } if *color == crate::drawing_ui::DIM_INK => "DIMENSIONS",
        Prim::Line { w, .. } | Prim::Arc { w, .. } if *w >= 0.45 => "VISIBLE",
        _ => "THIN",
    }
}

/// One entity (LINE, ARC, CIRCLE or TEXT) in DXF group codes.
fn dxf_entity(s: &mut String, p: &Prim, layer: &str) {
    let f = |v: f64| format!("{v:.4}");
    match p {
        Prim::Line { a, b, .. } | Prim::Chain { a, b, .. } => {
            s.push_str(&format!("0\nLINE\n8\n{layer}\n10\n{}\n20\n{}\n30\n0\n11\n{}\n21\n{}\n31\n0\n", f(a[0]), f(a[1]), f(b[0]), f(b[1])));
        }
        Prim::Arc { c, r, a0, a1, .. } => {
            if a1 - a0 >= std::f64::consts::TAU - 1e-9 {
                s.push_str(&format!("0\nCIRCLE\n8\n{layer}\n10\n{}\n20\n{}\n30\n0\n40\n{}\n", f(c[0]), f(c[1]), f(*r)));
            } else {
                s.push_str(&format!("0\nARC\n8\n{layer}\n10\n{}\n20\n{}\n30\n0\n40\n{}\n50\n{}\n51\n{}\n", f(c[0]), f(c[1]), f(*r), f(a0.to_degrees()), f(a1.to_degrees())));
            }
        }
        Prim::Text { at, size, text, anchor, .. } => {
            let h = match anchor {
                1 => 1,
                2 => 2,
                _ => 0,
            };
            s.push_str(&format!("0\nTEXT\n8\n{layer}\n10\n{}\n20\n{}\n30\n0\n40\n{:.3}\n1\n{}\n7\nSTANDARD\n72\n{h}\n11\n{}\n21\n{}\n31\n0\n", f(at[0]), f(at[1]), size, esc_dxf(text), f(at[0]), f(at[1])));
        }
        Prim::Dim(_) | Prim::End => {}
    }
}

/// DXF R12 (AC1009): a header, line types (HIDDEN, CENTER) and layers, circles and arcs kept as
/// CIRCLE and ARC, and each dimension as a DIMENSION entity whose picture is an anonymous block.
pub fn dxf(prims: &[Prim]) -> String {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in prims {
        if let Prim::Line { a, b, .. } = p {
            for q in [a, b] {
                lo = [lo[0].min(q[0]), lo[1].min(q[1])];
                hi = [hi[0].max(q[0]), hi[1].max(q[1])];
            }
        }
    }
    if !lo[0].is_finite() {
        (lo, hi) = ([0.0; 2], [297.0, 210.0]);
    }
    let mut s = String::new();
    s.push_str("0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n9\n$INSUNITS\n70\n4\n9\n$MEASUREMENT\n70\n1\n9\n$DWGCODEPAGE\n3\nANSI_1252\n9\n$LTSCALE\n40\n1.0\n");
    s.push_str(&format!("9\n$EXTMIN\n10\n{:.4}\n20\n{:.4}\n30\n0\n9\n$EXTMAX\n10\n{:.4}\n20\n{:.4}\n30\n0\n0\nENDSEC\n", lo[0], lo[1], hi[0], hi[1]));
    // tables: line types, layers, a text style and a dimension style
    s.push_str("0\nSECTION\n2\nTABLES\n");
    let ltypes: [(&str, &str, &[f64]); 3] = [("CONTINUOUS", "Solid line", &[]), ("HIDDEN", "Hidden __ __ __", &[3.0, -1.5]), ("CENTER", "Center ____ _ ____", &[6.0, -1.2, 1.0, -1.2])];
    s.push_str(&format!("0\nTABLE\n2\nLTYPE\n70\n{}\n", ltypes.len()));
    for (name, desc, pat) in ltypes {
        let total: f64 = pat.iter().map(|v| v.abs()).sum();
        s.push_str(&format!("0\nLTYPE\n2\n{name}\n70\n0\n3\n{desc}\n72\n65\n73\n{}\n40\n{total:.2}\n", pat.len()));
        for v in pat {
            s.push_str(&format!("49\n{v:.2}\n"));
        }
    }
    s.push_str("0\nENDTAB\n");
    // (name, colour, line type)
    let layers = [("0", 7, "CONTINUOUS"), ("VISIBLE", 7, "CONTINUOUS"), ("HIDDEN", 8, "HIDDEN"), ("CENTER", 1, "CENTER"), ("THIN", 8, "CONTINUOUS"), ("DIMENSIONS", 5, "CONTINUOUS"), ("TEXT", 7, "CONTINUOUS")];
    s.push_str(&format!("0\nTABLE\n2\nLAYER\n70\n{}\n", layers.len()));
    for (name, color, lt) in layers {
        s.push_str(&format!("0\nLAYER\n2\n{name}\n70\n0\n62\n{color}\n6\n{lt}\n"));
    }
    s.push_str("0\nENDTAB\n");
    s.push_str("0\nTABLE\n2\nSTYLE\n70\n1\n0\nSTYLE\n2\nSTANDARD\n70\n0\n40\n0.0\n41\n1.0\n50\n0.0\n71\n0\n42\n3.5\n3\ntxt\n4\n\n0\nENDTAB\n");
    s.push_str("0\nTABLE\n2\nDIMSTYLE\n70\n1\n0\nDIMSTYLE\n2\nSTANDARD\n70\n0\n41\n3.0\n140\n3.5\n0\nENDTAB\n");
    s.push_str("0\nENDSEC\n");
    // each dimension's picture goes into a block
    let mut blocks = String::new();
    let mut ents = String::new();
    let mut n = 0;
    let mut i = 0;
    while i < prims.len() {
        let Prim::Dim(info) = &prims[i] else {
            dxf_entity(&mut ents, &prims[i], dxf_layer(&prims[i]));
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < prims.len() && !matches!(prims[j], Prim::End) {
            j += 1;
        }
        if info.pts.len() < 2 {
            // nothing measured (e.g. parallel angle lines): plain entities
            for p in &prims[i + 1..j] {
                dxf_entity(&mut ents, p, "DIMENSIONS");
            }
            i = j + 1;
            continue;
        }
        n += 1;
        let name = format!("*D{n}");
        blocks.push_str(&format!("0\nBLOCK\n8\n0\n2\n{name}\n70\n1\n10\n0\n20\n0\n30\n0\n3\n{name}\n"));
        for p in &prims[i + 1..j] {
            dxf_entity(&mut blocks, p, "DIMENSIONS");
        }
        blocks.push_str("0\nENDBLK\n8\n0\n");
        let f = |v: f64| format!("{v:.4}");
        let (p1, p2) = (info.pts[0], info.pts[1]);
        let text = esc_dxf(&info.text);
        let head = |ty: u8, def: [f64; 2]| format!("0\nDIMENSION\n8\nDIMENSIONS\n2\n{name}\n3\nSTANDARD\n10\n{}\n20\n{}\n30\n0\n11\n{}\n21\n{}\n31\n0\n70\n{}\n1\n{text}\n", f(def[0]), f(def[1]), f(info.text_at[0]), f(info.text_at[1]), ty | 32);
        match info.kind {
            3 | 4 => {
                // radius: centre and the point on the circle; diameter: the two ends
                let (def, other) = if info.kind == 4 { ([2.0 * p1[0] - p2[0], 2.0 * p1[1] - p2[1]], p2) } else { (p1, p2) };
                ents.push_str(&head(info.kind, def));
                ents.push_str(&format!("15\n{}\n25\n{}\n35\n0\n40\n0\n", f(other[0]), f(other[1])));
            }
            5 if info.pts.len() == 4 => {
                let q = &info.pts;
                ents.push_str(&head(2, q[3]));
                ents.push_str(&format!("13\n{}\n23\n{}\n33\n0\n14\n{}\n24\n{}\n34\n0\n15\n{}\n25\n{}\n35\n0\n16\n{}\n26\n{}\n36\n0\n", f(q[0][0]), f(q[0][1]), f(q[1][0]), f(q[1][1]), f(q[2][0]), f(q[2][1]), f(info.line[0]), f(info.line[1])));
            }
            k => {
                let angle = match k {
                    1 => 0.0,
                    2 => 90.0,
                    _ => (p2[1] - p1[1]).atan2(p2[0] - p1[0]).to_degrees(),
                };
                ents.push_str(&head(if k == 0 { 1 } else { 0 }, info.line));
                ents.push_str(&format!("13\n{}\n23\n{}\n33\n0\n14\n{}\n24\n{}\n34\n0\n50\n{angle:.4}\n", f(p1[0]), f(p1[1]), f(p2[0]), f(p2[1])));
            }
        }
        i = j + 1;
    }
    s.push_str("0\nSECTION\n2\nBLOCKS\n");
    s.push_str(&blocks);
    s.push_str("0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    s.push_str(&ents);
    s.push_str("0\nENDSEC\n0\nEOF\n");
    s
}

fn esc_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn svg(prims: &[Prim], size: [f64; 2]) -> String {
    let [w, h] = size;
    let mut s = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}mm" height="{h}mm" viewBox="0 0 {w} {h}"><rect width="{w}" height="{h}" fill="white"/>"#);
    for p in prims {
        match p {
            Prim::Line { a, b, w: lw, dashed, color } => {
                s.push_str(&format!(
                    r#"<line x1="{:.3}" y1="{:.3}" x2="{:.3}" y2="{:.3}" stroke="rgb({},{},{})" stroke-width="{lw}" stroke-linecap="round"{}/>"#,
                    a[0],
                    h - a[1],
                    b[0],
                    h - b[1],
                    color[0],
                    color[1],
                    color[2],
                    if *dashed { r#" stroke-dasharray="1.5 1""# } else { "" }
                ));
            }
            Prim::Arc { c, r, a0, a1, w: lw, dashed, color } => {
                let pts: Vec<String> = Prim::arc_points(*c, *r, *a0, *a1).iter().map(|q| format!("{:.3},{:.3}", q[0], h - q[1])).collect();
                s.push_str(&format!(
                    r#"<polyline points="{}" fill="none" stroke="rgb({},{},{})" stroke-width="{lw}" stroke-linecap="round"{}/>"#,
                    pts.join(" "),
                    color[0],
                    color[1],
                    color[2],
                    if *dashed { r#" stroke-dasharray="1.5 1""# } else { "" }
                ));
            }
            Prim::Chain { a, b, w: lw, color } => {
                let [l, g, dot, g2] = Prim::CHAIN;
                s.push_str(&format!(
                    r#"<line x1="{:.3}" y1="{:.3}" x2="{:.3}" y2="{:.3}" stroke="rgb({},{},{})" stroke-width="{lw}" stroke-dasharray="{l} {g} {dot} {g2}"/>"#,
                    a[0],
                    h - a[1],
                    b[0],
                    h - b[1],
                    color[0],
                    color[1],
                    color[2]
                ));
            }
            Prim::Dim(_) | Prim::End => {}
            Prim::Text { at, size, text, anchor, color } => {
                let a = match anchor {
                    1 => "middle",
                    2 => "end",
                    _ => "start",
                };
                s.push_str(&format!(
                    r#"<text x="{:.3}" y="{:.3}" font-family="Helvetica, Arial, sans-serif" font-size="{size}" text-anchor="{a}" fill="rgb({},{},{})">{}</text>"#,
                    at[0],
                    h - at[1],
                    color[0],
                    color[1],
                    color[2],
                    esc_xml(text)
                ));
            }
        }
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pdf_is_well_formed() {
        let prims = vec![
            Prim::Line { a: [10.0, 10.0], b: [100.0, 10.0], w: 0.5, dashed: false, color: [0, 0, 0] },
            Prim::Text { at: [20.0, 20.0], size: 4.0, text: "Hi (there)".into(), anchor: 0, color: [0, 0, 0] },
        ];
        let p = pdf(&prims, [297.0, 210.0]);
        let s = String::from_utf8_lossy(&p);
        assert!(s.starts_with("%PDF-1.4") && s.ends_with("%%EOF\n"));
        // xref offsets point at "n 0 obj"
        let xref_at: usize = s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(s[xref_at..].starts_with("xref"));
        assert!(s.contains("Hi \\(there\\)"));
        assert!(dxf(&prims).contains("LINE"));
        // circles stay circles, layers carry line types, Ø is written the DXF way
        let prims = vec![
            Prim::Arc { c: [50.0, 50.0], r: 5.0, a0: 0.0, a1: std::f64::consts::TAU, w: 0.5, dashed: false, color: [0, 0, 0] },
            Prim::Arc { c: [50.0, 50.0], r: 8.0, a0: 0.0, a1: 1.0, w: 0.25, dashed: true, color: [90, 90, 90] },
            Prim::Chain { a: [40.0, 50.0], b: [60.0, 50.0], w: 0.18, color: [60, 60, 60] },
            Prim::Dim(crate::drawing_ui::DimInfo { kind: 4, pts: vec![[50.0, 50.0], [55.0, 50.0]], line: [60.0, 55.0], text_at: [62.0, 55.0], text: "2× Ø10".into() }),
            Prim::Line { a: [45.0, 50.0], b: [60.0, 55.0], w: 0.25, dashed: false, color: crate::drawing_ui::DIM_INK },
            Prim::Text { at: [62.0, 55.0], size: 3.5, text: "2× Ø10".into(), anchor: 0, color: crate::drawing_ui::DIM_INK },
            Prim::End,
        ];
        let d = dxf(&prims);
        for want in ["$ACADVER", "AC1009", "0\nCIRCLE\n8\nVISIBLE", "0\nARC\n8\nHIDDEN", "2\nHIDDEN\n70\n0", "2\nCENTER\n70\n0", "0\nLINE\n8\nCENTER", "0\nDIMENSION", "*D1", "%%c10", "\\U+00D7"] {
            assert!(d.contains(want), "{want}");
        }
        assert!(!d.contains('Ø'));
        assert!(svg(&prims, [297.0, 210.0]).contains("<line"));
    }
}

/// Rasterise sheet primitives to a PNG `width` pixels wide (for previews and agents).
pub fn png(prims: &[Prim], size: [f64; 2], width: u32) -> Result<Vec<u8>, String> {
    use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};
    let k = width as f64 / size[0];
    let height = (size[1] * k).round().max(1.0) as u32;
    let mut pm = Pixmap::new(width, height).ok_or("bad image size")?;
    pm.fill(Color::WHITE);
    // sheet mm (origin bottom-left) to pixels
    let at = |p: [f64; 2]| ((p[0] * k) as f32, ((size[1] - p[1]) * k) as f32);
    let face = ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0).map_err(|e| e.to_string())?;
    struct Out<'a> {
        pb: &'a mut PathBuilder,
        x: f32,
        y: f32,
        s: f32,
    }
    impl ttf_parser::OutlineBuilder for Out<'_> {
        fn move_to(&mut self, x: f32, y: f32) {
            self.pb.move_to(self.x + x * self.s, self.y - y * self.s);
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.pb.line_to(self.x + x * self.s, self.y - y * self.s);
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.pb.quad_to(self.x + x1 * self.s, self.y - y1 * self.s, self.x + x * self.s, self.y - y * self.s);
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.pb.cubic_to(self.x + x1 * self.s, self.y - y1 * self.s, self.x + x2 * self.s, self.y - y2 * self.s, self.x + x * self.s, self.y - y * self.s);
        }
        fn close(&mut self) {
            self.pb.close();
        }
    }
    // arcs and chain lines as plain line pieces
    let mut flat: Vec<Prim> = vec![];
    for p in prims {
        match p {
            Prim::Arc { c, r, a0, a1, w, dashed, color } => {
                for s in Prim::arc_points(*c, *r, *a0, *a1).windows(2) {
                    flat.push(Prim::Line { a: s[0], b: s[1], w: *w, dashed: *dashed, color: *color });
                }
            }
            Prim::Chain { a, b, w, color } => {
                for (p, q) in Prim::chain_pieces(*a, *b) {
                    flat.push(Prim::Line { a: p, b: q, w: *w, dashed: false, color: *color });
                }
            }
            _ => {}
        }
    }
    for p in prims.iter().chain(flat.iter()) {
        match p {
            Prim::Arc { .. } | Prim::Chain { .. } | Prim::Dim(_) | Prim::End => {}
            Prim::Line { a, b, w, dashed, color } => {
                let mut pb = PathBuilder::new();
                let (a, b) = (at(*a), at(*b));
                pb.move_to(a.0, a.1);
                pb.line_to(b.0, b.1);
                let Some(path) = pb.finish() else { continue };
                let mut paint = Paint::default();
                paint.set_color_rgba8(color[0], color[1], color[2], 255);
                paint.anti_alias = true;
                let mut st = Stroke { width: ((*w * k) as f32).max(0.8), line_cap: LineCap::Round, ..Default::default() };
                if *dashed {
                    st.dash = StrokeDash::new(vec![(1.5 * k) as f32, (1.0 * k) as f32], 0.0);
                }
                pm.stroke_path(&path, &paint, &st, Transform::identity(), None);
            }
            Prim::Text { at: pos, size: sz, text, anchor, color } => {
                let s = (*sz * k) as f32 / face.units_per_em() as f32 * 1.35;
                let adv: f32 = text.chars().map(|c| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).unwrap_or(500) as f32 * s).sum();
                let (mut x, y) = at(*pos);
                x -= match anchor {
                    1 => adv / 2.0,
                    2 => adv,
                    _ => 0.0,
                };
                let mut pb = PathBuilder::new();
                for c in text.chars() {
                    let Some(g) = face.glyph_index(c) else {
                        x += 500.0 * s;
                        continue;
                    };
                    face.outline_glyph(g, &mut Out { pb: &mut pb, x, y, s });
                    x += face.glyph_hor_advance(g).unwrap_or(500) as f32 * s;
                }
                if let Some(path) = pb.finish() {
                    let mut paint = Paint::default();
                    paint.set_color_rgba8(color[0], color[1], color[2], 255);
                    paint.anti_alias = true;
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
    }
    pm.encode_png().map_err(|e| e.to_string())
}
