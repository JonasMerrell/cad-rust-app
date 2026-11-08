//! Annotate: give the schematic's parts references in order of their place on the sheets, like
//! KiCad's Annotate Schematic (R1, R2, … left to right or top to bottom).

use crate::pcb::Pcb;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Order {
    /// Down each column, columns left to right (KiCad's "sort by X position").
    #[default]
    Columns,
    /// Along each row, rows top to bottom ("sort by Y position").
    Rows,
}

/// The letters a reference starts with (`R12` → `R`, `U?` → `U`, `SW3` → `SW`).
pub fn prefix(r: &str) -> &str {
    let end = r.find(|c: char| c.is_ascii_digit() || c == '?').unwrap_or(r.len());
    let p = &r[..end];
    if p.is_empty() { "U" } else { p }
}

fn number(r: &str) -> Option<u32> {
    let p = prefix(r);
    r.get(p.len()..).and_then(|n| n.parse().ok()).filter(|n| *n > 0)
}

/// Annotate. With `keep`, parts that already have a unique number keep it and only the others
/// (`R?`, repeats) get the next free numbers in order; otherwise every part is numbered afresh.
/// Variants follow the renamed parts. Returns the changes as (old, new).
pub fn annotate(p: &mut Pcb, order: Order, keep: bool) -> Vec<(String, String)> {
    // sheet first (sheets sit side by side in x), then the order asked for; positions to 1.27 mm
    let grid = |v: f64| (v / 1.27).round() as i64;
    let mut idx: Vec<usize> = (0..p.parts.len()).collect();
    idx.sort_by_key(|&i| {
        let at = p.parts[i].sch;
        let sheet = crate::pcb_sheet::sheet_at(at);
        let (x, y) = (grid(at[0]), -grid(at[1]));
        match order {
            Order::Columns => (sheet, x, y),
            Order::Rows => (sheet, y, x),
        }
    });
    let mut taken: std::collections::HashSet<(String, u32)> = Default::default();
    let mut fixed = vec![false; p.parts.len()];
    if keep {
        for &i in &idx {
            let r = &p.parts[i].reference;
            if let Some(n) = number(r) {
                if taken.insert((prefix(r).to_uppercase(), n)) {
                    fixed[i] = true;
                }
            }
        }
    }
    let mut new_refs: Vec<String> = p.parts.iter().map(|x| x.reference.clone()).collect();
    for &i in &idx {
        if fixed[i] {
            continue;
        }
        let pre = prefix(&p.parts[i].reference).to_string();
        let mut n = 1;
        while taken.contains(&(pre.to_uppercase(), n)) {
            n += 1;
        }
        taken.insert((pre.to_uppercase(), n));
        new_refs[i] = format!("{pre}{n}");
    }
    let changes: Vec<(usize, String, String)> = p.parts.iter().enumerate().filter(|(i, x)| x.reference != new_refs[*i]).map(|(i, x)| (i, x.reference.clone(), new_refs[i].clone())).collect();
    // variants follow, through temporary names so R1 → R2 and R2 → R1 don't collide
    for (i, old, _) in &changes {
        p.rename_ref(old, &format!("\u{1}{i}"));
    }
    for (i, _, new) in &changes {
        p.rename_ref(&format!("\u{1}{i}"), new);
        p.parts[*i].reference = new.clone();
    }
    changes.into_iter().map(|(_, a, b)| (a, b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Pcb {
        let mut p = Pcb::new("t");
        for (r, at) in [("R?", [30.0, 0.0]), ("R7", [10.0, 0.0]), ("R7", [10.0, -20.0]), ("C1", [0.0, 0.0]), ("SW?", [5.0, 5.0])] {
            p.parts.push(crate::pcb::Part { id: p.parts.len() as u64 + 1, reference: r.into(), symbol: "R".into(), footprint: "R_0805".into(), sch: at, ..Default::default() });
        }
        p
    }

    #[test]
    fn renumbers_in_columns_and_keeps_numbers() {
        let mut p = board();
        let ch = annotate(&mut p, Order::Columns, false);
        let refs: Vec<&str> = p.parts.iter().map(|x| x.reference.as_str()).collect();
        // columns left to right, top to bottom in each: R7 (top, x 10), R7 (below), R? (x 30)
        assert_eq!(refs, ["R3", "R1", "R2", "C1", "SW1"]);
        assert!(!ch.is_empty());
        let mut p = board();
        annotate(&mut p, Order::Columns, true);
        let refs: Vec<&str> = p.parts.iter().map(|x| x.reference.as_str()).collect();
        // the first R7 keeps its number, the repeat and R? take the free ones in order
        assert_eq!(refs, ["R2", "R7", "R1", "C1", "SW1"]);
    }

    #[test]
    fn variants_follow_swapped_references() {
        let mut p = board();
        p.parts[1].reference = "R1".into();
        p.parts[2].reference = "R2".into();
        p.parts[0].reference = "R3".into();
        p.variants.push(crate::pcb_sheet::Variant { name: "lite".into(), dnp: vec!["R2".into()], ..Default::default() });
        // rows: R2 (y 0 row… ) ordering puts the y = 0 row first: C1, R1 (x 10), R3 (x 30), then R2
        annotate(&mut p, Order::Rows, false);
        let r_of = |id: u64| p.parts.iter().find(|x| x.id == id).unwrap().reference.clone();
        assert_eq!(p.variants[0].dnp, vec![r_of(3)]);
    }
}
