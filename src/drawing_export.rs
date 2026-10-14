//! Drawing exporters: PDF (hand-written, vector), DXF (R12 ASCII) and SVG.
//! All take sheet primitives in millimetres with the origin at the bottom left.

use crate::drawing_ui::Prim;

fn esc_pdf(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            c if c.is_ascii() => c.to_string(),
            _ => "?".into(),
        })
        .collect()
}

pub fn pdf(prims: &[Prim], size: [f64; 2]) -> Vec<u8> {
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
            Prim::Text { at, size, text, anchor, color } => {
                let pt = size * k;
                // Helvetica averages ~0.5 em per character
                let wdt = text.chars().count() as f64 * pt * 0.5;
                let x = at[0] * k - match anchor {
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
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>", size[0] * k, size[1] * k),
        format!("<< /Length {} >>\nstream\n{}endstream", c.len(), c),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
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

pub fn dxf(prims: &[Prim]) -> String {
    let mut s = String::from("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    for p in prims {
        match p {
            Prim::Line { a, b, dashed, .. } => {
                s.push_str(&format!(
                    "0\nLINE\n8\n{}\n10\n{:.4}\n20\n{:.4}\n30\n0\n11\n{:.4}\n21\n{:.4}\n31\n0\n",
                    if *dashed { "HIDDEN" } else { "0" },
                    a[0],
                    a[1],
                    b[0],
                    b[1]
                ));
            }
            Prim::Text { at, size, text, anchor, .. } => {
                let h = match anchor {
                    1 => 1,
                    2 => 2,
                    _ => 0,
                };
                s.push_str(&format!("0\nTEXT\n8\nTEXT\n10\n{:.4}\n20\n{:.4}\n30\n0\n40\n{:.3}\n1\n{}\n72\n{h}\n11\n{:.4}\n21\n{:.4}\n31\n0\n", at[0], at[1], size, text, at[0], at[1]));
            }
        }
    }
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
        assert!(svg(&prims, [297.0, 210.0]).contains("<line"));
    }
}
