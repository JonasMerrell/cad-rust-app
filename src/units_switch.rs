//! Changing a document's units without changing its geometry.
//!
//! Plain numbers in length fields are read in the document's units, so switching
//! mm → in would turn an 8 mm extrude into 8 in. Before the switch every length
//! expression whose value depends on the units ("8", "2 * 4") is rewritten so it
//! keeps its size: either converted to the new unit ("0.31496063") or given the
//! old unit explicitly ("8 mm").

use crate::doc::{Document, Op};
use crate::expr::{self, Env, Kind, Units};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitChange {
    /// Rewrite plain numbers in the new unit: 8 (mm) becomes 0.31496063 (in).
    Convert,
    /// Keep the numbers and name the old unit: 8 becomes "8 mm".
    Label,
    /// Read the same numbers in the new unit: the model changes size.
    Rescale,
}

/// Switch `doc` to `to`, keeping every length the same size unless `how` is Rescale.
/// Returns how many values were rewritten.
pub fn switch_units(doc: &mut Document, to: Units, how: UnitChange) -> usize {
    let from = doc.units;
    let mut n = 0;
    if from != to && how != UnitChange::Rescale {
        // Variables first, one at a time so each is read with the ones before it already
        // rewritten. A plain-number variable gets the old unit written out ("8" becomes "8 mm"):
        // that keeps its size as a length and its number where a count or an angle field reads
        // it, which a converted number would not.
        let label = |s: &mut String, pairs: &[(String, String)]| -> bool {
            let (vars, _) = expr::resolve_vars(pairs, from);
            match rewrite(s, &vars, from, to, UnitChange::Label) {
                Some(t) => {
                    *s = t;
                    true
                }
                None => false,
            }
        };
        let mut pairs: Vec<(String, String)> = vec![];
        for v in &mut doc.variables {
            if label(&mut v.expr, &pairs) {
                n += 1;
            }
            pairs.push((v.name.clone(), v.expr.clone()));
        }
        for c in &mut doc.configs {
            let mut pairs: Vec<(String, String)> = vec![];
            for v in &doc.variables {
                let e = match c.values.get_mut(&v.name) {
                    Some(e) => {
                        if label(e, &pairs) {
                            n += 1;
                        }
                        e.clone()
                    }
                    None => v.expr.clone(),
                };
                pairs.push((v.name.clone(), e));
            }
        }
        let (vars, _) = doc.var_values();
        let mut fix = |s: &mut String| {
            if let Some(t) = rewrite(s, &vars, from, to, how) {
                *s = t;
                n += 1;
            }
        };
        let defs = doc.custom_features.clone();
        for f in &mut doc.features {
            for_each_length(&mut f.op, &defs, &mut fix);
        }
    }
    doc.units = to;
    n
}

/// Every length expression of a feature (sketch dimensions included).
fn for_each_length(op: &mut Op, defs: &[crate::script::CustomDef], fix: &mut impl FnMut(&mut String)) {
    match op {
        Op::Sketch { sketch, .. } => {
            for c in &mut sketch.constraints {
                let angle = c.is_angle();
                if let Some(d) = c.dim_mut() {
                    if !angle {
                        fix(&mut d.expr);
                    }
                }
            }
        }
        Op::Plane { offset, .. } => fix(offset),
        Op::Extrude { depth, depth2, .. } => {
            fix(depth);
            fix(depth2);
        }
        Op::Fillet { radius, end_radius, .. } => {
            fix(radius);
            fix(end_radius);
        }
        Op::Chamfer { distance, distance2, .. } => {
            fix(distance);
            fix(distance2);
        }
        Op::Shell { thickness, .. } | Op::Thicken { thickness, .. } | Op::ConvertSheet { thickness, .. } | Op::Rib { thickness, .. } => fix(thickness),
        Op::Hole { diameter, depth, cb_diameter, cb_depth, cs_diameter, .. } => {
            for s in [diameter, depth, cb_diameter, cb_depth, cs_diameter] {
                fix(s);
            }
        }
        Op::LinearPattern { spacing, spacing2, .. } => {
            fix(spacing);
            fix(spacing2);
        }
        Op::Helix { radius, pitch, .. } => {
            fix(radius);
            fix(pitch);
        }
        Op::Transform { distance, .. } | Op::OffsetSurface { distance, .. } | Op::MoveFace { distance, .. } => fix(distance),
        Op::SurfaceExtrude { depth, .. } => fix(depth),
        Op::Flange { length, inset, .. } => {
            fix(length);
            fix(inset);
        }
        Op::Heal { tolerance, .. } => fix(tolerance),
        Op::SheetMetal { thickness, radius, depth, .. } => {
            for s in [thickness, radius, depth] {
                fix(s);
            }
        }
        Op::Custom { name, values, .. } => {
            // parameters left at their default use the definition's text, so pin those too
            if let Some(def) = defs.iter().find(|d| d.name == *name) {
                for p in def.params.iter().filter(|p| p.kind == "length") {
                    let mut s = values.get(&p.name).cloned().unwrap_or_else(|| p.default.clone());
                    let before = s.clone();
                    fix(&mut s);
                    if s != before {
                        values.insert(p.name.clone(), s);
                    }
                }
            }
        }
        _ => {}
    }
}

/// The rewritten text of one length expression, or None when it does not depend on the units
/// (it names a unit or a variable, or does not evaluate).
pub fn rewrite(src: &str, vars: &expr::Vars, from: Units, to: Units, how: UnitChange) -> Option<String> {
    let at = |u: Units| expr::eval(src, Kind::Length, &Env { vars, units: u }).ok();
    let (a, b) = (at(Units::Mm)?, at(Units::In)?);
    // a value that is the same read in mm and in inches carries its own unit
    if (a - b).abs() <= 1e-12 * a.abs().max(1.0) {
        return None;
    }
    let src = src.trim();
    if how == UnitChange::Convert {
        if let Ok(v) = src.parse::<f64>() {
            let nv = v * from.scale() / to.scale();
            let t = expr::trim(nv, 9);
            // only when the new number is exact enough to keep the size
            if let Ok(back) = t.parse::<f64>() {
                if (back * to.scale() - v * from.scale()).abs() <= 1e-7 * (v * from.scale()).abs().max(1.0) {
                    return Some(t);
                }
            }
        }
    }
    let simple = src.parse::<f64>().is_ok();
    Some(if simple { format!("{src} {}", from.label()) } else { format!("({src}) {}", from.label()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(depth: &str) -> Document {
        let mut d = Document::default();
        let op = Op::new_default("extrude").unwrap();
        d.insert("Extrude 1".into(), op);
        if let Op::Extrude { depth: dd, .. } = &mut d.features[0].op {
            *dd = depth.into();
        }
        d
    }

    fn depth_mm(d: &Document) -> f64 {
        let (vars, _) = d.var_values();
        let Op::Extrude { depth, .. } = &d.features[0].op else { panic!() };
        crate::doc::eval(depth, Kind::Length, &vars, d.units).unwrap()
    }

    #[test]
    fn switching_units_keeps_sizes() {
        for how in [UnitChange::Convert, UnitChange::Label] {
            for src in ["8", "2 * 4", "8 mm", "0.5 in"] {
                let mut d = doc_with(src);
                let before = depth_mm(&d);
                switch_units(&mut d, Units::In, how);
                assert!((depth_mm(&d) - before).abs() < 1e-6, "{src} {how:?}");
                switch_units(&mut d, Units::Cm, how);
                assert!((depth_mm(&d) - before).abs() < 1e-6, "{src} {how:?} cm");
            }
        }
        // the old behaviour is still there when asked for
        let mut d = doc_with("8");
        switch_units(&mut d, Units::In, UnitChange::Rescale);
        assert!((depth_mm(&d) - 8.0 * 25.4).abs() < 1e-9);
    }

    #[test]
    fn rewrites_read_well() {
        let vars = expr::Vars::from(std::collections::HashMap::from([("w".to_string(), 40.0)]));
        assert_eq!(rewrite("8", &vars, Units::Mm, Units::Cm, UnitChange::Convert).as_deref(), Some("0.8"));
        assert_eq!(rewrite("8", &vars, Units::Mm, Units::In, UnitChange::Label).as_deref(), Some("8 mm"));
        assert_eq!(rewrite("2*4", &vars, Units::Mm, Units::In, UnitChange::Convert).as_deref(), Some("(2*4) mm"));
        // variables and explicit units already keep their size
        assert_eq!(rewrite("w / 2", &vars, Units::Mm, Units::In, UnitChange::Convert), None);
        assert_eq!(rewrite("3 mm", &vars, Units::Mm, Units::In, UnitChange::Convert), None);
        assert_eq!(rewrite("bad +", &vars, Units::Mm, Units::In, UnitChange::Convert), None);
    }

    #[test]
    fn variables_and_sketch_dims_follow() {
        let mut d = doc_with("w");
        d.variables.push(crate::doc::Variable { name: "w".into(), expr: "8".into() });
        let before = depth_mm(&d);
        switch_units(&mut d, Units::In, UnitChange::Convert);
        assert!((depth_mm(&d) - before).abs() < 1e-6);
        assert_eq!(d.variables[0].expr, "8 mm");
        // angle fields are left alone
        let mut d = Document::default();
        let mut op = Op::new_default("revolve").unwrap();
        if let Op::Revolve { angle, .. } = &mut op {
            *angle = "90".into();
        }
        d.insert("Revolve 1".into(), op);
        switch_units(&mut d, Units::In, UnitChange::Convert);
        let Op::Revolve { angle, .. } = &d.features[0].op else { panic!() };
        assert_eq!(angle, "90");
    }

    #[test]
    fn plain_variables_keep_sizes_and_counts() {
        for how in [UnitChange::Convert, UnitChange::Label] {
            let mut d = doc_with("w + 1");
            d.variables.push(crate::doc::Variable { name: "w".into(), expr: "8".into() });
            d.variables.push(crate::doc::Variable { name: "h".into(), expr: "w + 2".into() });
            d.variables.push(crate::doc::Variable { name: "n".into(), expr: "4".into() });
            d.variables.push(crate::doc::Variable { name: "a".into(), expr: "30".into() });
            let read = |d: &Document, src: &str, kind: Kind| {
                let (vars, _) = d.var_values();
                crate::doc::eval(src, kind, &vars, d.units).unwrap()
            };
            let before = depth_mm(&d);
            assert_eq!(before, 9.0);
            for to in [Units::In, Units::Cm, Units::Mm] {
                switch_units(&mut d, to, how);
                assert!((depth_mm(&d) - before).abs() < 1e-6, "{how:?} {to:?}");
                assert!((read(&d, "h", Kind::Length) - 10.0).abs() < 1e-6, "{how:?} {to:?}");
                assert_eq!(read(&d, "n", Kind::Count), 4.0);
                assert_eq!(read(&d, "n + 1", Kind::Count), 5.0);
                assert!((read(&d, "a", Kind::Angle) - 30.0).abs() < 1e-9);
            }
        }
    }
}
