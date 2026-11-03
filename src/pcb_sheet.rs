//! Multi-sheet schematics and assembly variants (Altium's hierarchical design and variants).
//!
//! Sheets are side by side in schematic space: sheet k owns the strip of x within half a pitch
//! of `k * SHEET_PITCH`. Wires, pins and labels on different sheets then never touch, and every
//! schematic tool works on a sheet unchanged. A sheet symbol on a parent sheet stands for a child
//! sheet; its entries carry the child's ports up to wires on the parent.
//!
//! Variants mark parts not fitted, or fit them with another value or part number, for the BOM,
//! pick and place, assembly drawings, 3D model and simulation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pcb::{Label, LabelKind, Pcb};
use crate::pcb_lib::P;

/// Schematic x distance between sheet origins, mm.
pub const SHEET_PITCH: f64 = 2000.0;
/// Spacing of sheet entries along a symbol's side, mm.
pub const ENTRY_PITCH: f64 = 2.54;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Sheet {
    pub name: String,
}

/// A child sheet drawn as a box on its parent, with an entry per port of the child.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SheetSym {
    /// The child sheet's index.
    pub sheet: usize,
    /// Top left corner (schematic coordinates, on the parent sheet).
    pub at: P,
    #[serde(default = "default_width")]
    pub width: f64,
    /// Entries, top to bottom on each side.
    #[serde(default)]
    pub entries: Vec<SheetEntry>,
}

fn default_width() -> f64 {
    15.24
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SheetEntry {
    pub name: String,
    /// On the right side (outputs, by habit); left otherwise.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub right: bool,
}

impl SheetSym {
    /// Entry rows on the taller side.
    fn rows(&self) -> usize {
        let r = self.entries.iter().filter(|e| e.right).count();
        r.max(self.entries.len() - r).max(1)
    }
    pub fn height(&self) -> f64 {
        (self.rows() + 1) as f64 * ENTRY_PITCH
    }
    /// The box, (low, high) corners.
    pub fn rect(&self) -> (P, P) {
        let h = self.height();
        ([self.at[0], self.at[1] - h], [self.at[0] + self.width, self.at[1]])
    }
    /// Where entry `i` meets wires: on the box edge, on the 2.54 grid down the side.
    pub fn entry_at(&self, i: usize) -> P {
        let e = &self.entries[i];
        let k = self.entries[..i].iter().filter(|x| x.right == e.right).count();
        [if e.right { self.at[0] + self.width } else { self.at[0] }, self.at[1] - (k + 1) as f64 * ENTRY_PITCH]
    }
    pub fn contains(&self, q: P) -> bool {
        let (lo, hi) = self.rect();
        q[0] >= lo[0] && q[0] <= hi[0] && q[1] >= lo[1] && q[1] <= hi[1]
    }
}

/// An assembly variant: parts left off, put back, or fitted differently.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Variant {
    pub name: String,
    /// References not fitted in this variant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dnp: Vec<String>,
    /// References fitted in this variant though the base design leaves them off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fitted: Vec<String>,
    /// Another value for a reference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    /// Another manufacturer part number for a reference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mpns: BTreeMap<String, String>,
}

/// The sheet a schematic point is on.
pub fn sheet_at(q: P) -> usize {
    ((q[0] + SHEET_PITCH / 2.0) / SHEET_PITCH).floor().max(0.0) as usize
}

/// The centre of sheet k's strip.
pub fn origin(k: usize) -> P {
    [k as f64 * SHEET_PITCH, 0.0]
}

/// Keep only drawing items on sheet k (by their first point).
pub fn on_sheet(list: Vec<crate::pcb_export::Draw>, k: usize) -> Vec<crate::pcb_export::Draw> {
    list.into_iter().filter(|d| crate::pcb_export::first_point(&d.prim).is_none_or(|q| sheet_at(q) == k)).collect()
}

impl Pcb {
    /// Number of sheets (at least one).
    pub fn sheet_count(&self) -> usize {
        self.sheets.len().max(1)
    }
    pub fn sheet_name(&self, k: usize) -> String {
        self.sheets.get(k).map(|s| s.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| if k == 0 { "Main".into() } else { format!("Sheet {}", k + 1) })
    }
    /// The sheet index for a name or number ("Power", "2"), if there is one.
    pub fn find_sheet(&self, s: &str) -> Option<usize> {
        let s = s.trim();
        (0..self.sheet_count()).find(|&k| self.sheet_name(k).eq_ignore_ascii_case(s)).or_else(|| s.parse::<usize>().ok().filter(|&k| k >= 1 && k <= self.sheet_count()).map(|k| k - 1))
    }
    /// Add a sheet, returning its index.
    pub fn add_sheet(&mut self, name: &str) -> usize {
        if self.sheets.is_empty() {
            self.sheets.push(Sheet { name: "Main".into() });
        }
        let names: Vec<String> = (0..self.sheet_count()).map(|k| self.sheet_name(k)).collect();
        let mut n = if name.trim().is_empty() { format!("Sheet {}", self.sheets.len() + 1) } else { name.trim().to_string() };
        let base = n.clone();
        let mut i = 2;
        while names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
            n = format!("{base} {i}");
            i += 1;
        }
        self.sheets.push(Sheet { name: n });
        self.sheets.len() - 1
    }
    /// Whether a sheet holds anything.
    pub fn sheet_used(&self, k: usize) -> bool {
        self.parts.iter().any(|x| sheet_at(x.sch) == k)
            || self.wires.iter().any(|w| sheet_at(w.a) == k)
            || self.labels.iter().any(|l| sheet_at(l.at) == k)
            || self.sheet_symbols.iter().any(|s| sheet_at(s.at) == k)
    }
    /// Remove an empty sheet (not the first), shifting later sheets' contents left.
    pub fn remove_sheet(&mut self, k: usize) -> Result<(), String> {
        if k == 0 || k >= self.sheet_count() {
            return Err("the first sheet stays".into());
        }
        if self.sheet_used(k) {
            return Err(format!("{} isn't empty", self.sheet_name(k)));
        }
        let shift = |q: &mut P| {
            if sheet_at(*q) > k {
                q[0] -= SHEET_PITCH;
            }
        };
        for x in &mut self.parts {
            shift(&mut x.sch);
        }
        for w in &mut self.wires {
            let s = sheet_at(w.a) > k;
            if s {
                w.a[0] -= SHEET_PITCH;
                w.b[0] -= SHEET_PITCH;
            }
        }
        for l in &mut self.labels {
            shift(&mut l.at);
        }
        for q in &mut self.no_connects {
            shift(q);
        }
        self.sheet_symbols.retain(|s| s.sheet != k);
        for s in &mut self.sheet_symbols {
            shift(&mut s.at);
            if s.sheet > k {
                s.sheet -= 1;
            }
        }
        self.sheets.remove(k);
        Ok(())
    }
    /// Hierarchical design: net labels are local to their sheet, and ports reach the parent
    /// through sheet entries. Without sheet symbols every label and port is global (flat).
    pub fn hierarchical(&self) -> bool {
        !self.sheet_symbols.is_empty()
    }
    /// Names of supply and ground ports: net labels with these names join them on every sheet.
    pub fn supply_names(&self) -> std::collections::HashSet<&str> {
        self.labels.iter().filter(|l| matches!(l.kind, LabelKind::Power | LabelKind::Ground)).map(|l| l.text.trim()).filter(|t| !t.is_empty()).collect()
    }
    /// The sheet symbol that stands for sheet k.
    pub fn symbol_of(&self, k: usize) -> Option<usize> {
        self.sheet_symbols.iter().position(|s| s.sheet == k)
    }
    /// The port names on sheet k, sorted.
    pub fn ports_on(&self, k: usize) -> Vec<String> {
        let mut v: Vec<String> = self.labels.iter().filter(|l| l.kind == LabelKind::Port && sheet_at(l.at) == k && !l.text.trim().is_empty()).map(|l| l.text.trim().to_string()).collect();
        v.sort();
        v.dedup();
        v
    }
    /// Give each sheet symbol an entry per port of its sheet (new ones on the left), and drop
    /// entries whose port is gone. Returns whether anything changed.
    pub fn sync_entries(&mut self) -> bool {
        let mut changed = false;
        for i in 0..self.sheet_symbols.len() {
            let ports = self.ports_on(self.sheet_symbols[i].sheet);
            let s = &mut self.sheet_symbols[i];
            let before = s.entries.len();
            s.entries.retain(|e| ports.contains(&e.name));
            changed |= s.entries.len() != before;
            for p in ports {
                if !s.entries.iter().any(|e| e.name == p) {
                    s.entries.push(SheetEntry { name: p, right: false });
                    changed = true;
                }
            }
        }
        changed
    }
    /// Place a symbol for sheet `child` on sheet `parent` at `at` (top left), with its entries.
    pub fn add_sheet_symbol(&mut self, child: usize, at: P) -> Result<usize, String> {
        if child == 0 || child >= self.sheet_count() {
            return Err("the first sheet is the top of the hierarchy; pick another".into());
        }
        if let Some(i) = self.symbol_of(child) {
            return Err(format!("{} already has a symbol (on {})", self.sheet_name(child), self.sheet_name(sheet_at(self.sheet_symbols[i].at))));
        }
        let parent = sheet_at(at);
        // no loops: the parent may not sit below the child
        let mut up = Some(parent);
        while let Some(s) = up {
            if s == child {
                return Err(format!("{} is inside {}: a sheet can't contain itself", self.sheet_name(parent), self.sheet_name(child)));
            }
            up = self.symbol_of(s).map(|i| sheet_at(self.sheet_symbols[i].at));
        }
        self.sheet_symbols.push(SheetSym { sheet: child, at, width: default_width(), entries: vec![] });
        self.sync_entries();
        Ok(self.sheet_symbols.len() - 1)
    }
    /// The parent of sheet k (None for top sheets).
    pub fn parent_of(&self, k: usize) -> Option<usize> {
        self.symbol_of(k).map(|i| sheet_at(self.sheet_symbols[i].at))
    }
    /// A net name for a label on sheet k: a sheet-local name also used on another sheet carries
    /// its sheet's name (below the first sheet), so two sheets' "SDA" stay two nets.
    pub fn label_net_name(&self, l: &Label) -> String {
        let t = l.text.trim().to_string();
        let k = sheet_at(l.at);
        let supply = self.supply_names();
        let local = |x: &Label| x.kind == LabelKind::Port || (x.kind == LabelKind::Net && !supply.contains(x.text.trim()));
        let clash = || self.labels.iter().any(|m| local(m) && m.text.trim() == t && sheet_at(m.at) != k);
        if self.hierarchical() && k != 0 && local(l) && clash() {
            format!("{}/{}", self.sheet_name(k), t)
        } else {
            t
        }
    }

    // ------------------------------------------------------------ variants

    pub fn variant_index(&self, name: &str) -> Option<usize> {
        self.variants.iter().position(|v| v.name.eq_ignore_ascii_case(name.trim()))
    }
    fn active(&self) -> Option<&Variant> {
        if self.variant.is_empty() {
            None
        } else {
            self.variant_index(&self.variant).map(|i| &self.variants[i])
        }
    }
    /// Whether part i is fitted in the active variant.
    pub fn fitted(&self, i: usize) -> bool {
        let part = &self.parts[i];
        match self.active() {
            Some(v) if v.dnp.iter().any(|r| r == &part.reference) => false,
            Some(v) if v.fitted.iter().any(|r| r == &part.reference) => true,
            _ => !part.dnp,
        }
    }
    /// Part i's value in the active variant.
    pub fn value_of(&self, i: usize) -> String {
        let part = &self.parts[i];
        self.active().and_then(|v| v.values.get(&part.reference)).cloned().unwrap_or_else(|| part.value.clone())
    }
    pub fn mpn_of(&self, i: usize) -> String {
        let part = &self.parts[i];
        self.active().and_then(|v| v.mpns.get(&part.reference)).cloned().unwrap_or_else(|| part.mpn.clone())
    }
    /// A copy as the variant builds it: parts not fitted dropped from the parts list's outputs
    /// (marked `dnp`), others with their variant value and part number. "" is the base design.
    pub fn as_variant(&self, name: &str) -> Pcb {
        let mut p = self.clone();
        p.variant = if name.is_empty() || self.variant_index(name).is_none() { String::new() } else { name.to_string() };
        for i in 0..p.parts.len() {
            let (fit, value, mpn) = (p.fitted(i), p.value_of(i), p.mpn_of(i));
            let part = &mut p.parts[i];
            part.dnp = !fit;
            part.value = value;
            part.mpn = mpn;
        }
        p.variant.clear();
        p
    }
    /// Set whether part i is fitted in the active variant (or the base design).
    pub fn set_fitted(&mut self, i: usize, fit: bool) {
        let r = self.parts[i].reference.clone();
        let base = !self.parts[i].dnp;
        let Some(vi) = (if self.variant.is_empty() { None } else { self.variant_index(&self.variant) }) else {
            self.parts[i].dnp = !fit;
            return;
        };
        let v = &mut self.variants[vi];
        v.dnp.retain(|x| x != &r);
        v.fitted.retain(|x| x != &r);
        if fit != base {
            if fit {
                v.fitted.push(r);
            } else {
                v.dnp.push(r);
            }
        }
    }
    /// Set part i's value in the active variant ("" or the base value clears the override).
    pub fn set_variant_value(&mut self, i: usize, value: &str) {
        let r = self.parts[i].reference.clone();
        let base = self.parts[i].value.clone();
        let Some(vi) = (if self.variant.is_empty() { None } else { self.variant_index(&self.variant) }) else {
            self.parts[i].value = value.to_string();
            return;
        };
        let v = &mut self.variants[vi];
        if value.trim().is_empty() || value == base {
            v.values.remove(&r);
        } else {
            v.values.insert(r, value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcb::{Label, LabelKind, Wire};

    #[test]
    fn sheets_split_space() {
        assert_eq!(sheet_at([0.0, 0.0]), 0);
        assert_eq!(sheet_at([-50.0, 0.0]), 0);
        assert_eq!(sheet_at([SHEET_PITCH + 30.0, 10.0]), 1);
        assert_eq!(sheet_at([SHEET_PITCH * 1.49, 0.0]), 1);
        let mut p = Pcb::new("S");
        assert_eq!(p.sheet_count(), 1);
        assert_eq!(p.add_sheet("Power"), 1);
        assert_eq!(p.add_sheet("power"), 2);
        assert_eq!(p.sheet_name(2), "power 2");
        assert_eq!(p.find_sheet("POWER"), Some(1));
        assert_eq!(p.find_sheet("3"), Some(2));
        p.labels.push(Label { at: [2.0 * SHEET_PITCH + 5.0, 0.0], text: "X".into(), kind: LabelKind::Net, rot: 0 });
        assert!(p.remove_sheet(2).is_err());
        assert!(p.remove_sheet(1).is_ok());
        assert_eq!(sheet_at(p.labels[0].at), 1, "later sheets move down");
        assert_eq!(p.sheet_name(1), "power 2");
    }

    /// A resistor on a child sheet, its pin 1 on port IN; the parent wires IN to a connector.
    #[test]
    fn ports_connect_through_sheet_entries() {
        let mut p = Pcb::new("H");
        let j = p.add_part("J", Some("PinHeader_1x02"), None, [0.0, 0.0]).unwrap();
        let child = p.add_sheet("Filter");
        let r = p.add_part("R", None, None, [0.0, 0.0]).unwrap();
        p.parts[r].sch = crate::pcb::add(origin(child), [0.0, 0.0]);
        let pins = p.sch_pins();
        let r1 = pins.iter().find(|x| x.part == r && x.num == "1").unwrap().at;
        let r2 = pins.iter().find(|x| x.part == r && x.num == "2").unwrap().at;
        let j1 = pins.iter().find(|x| x.part == j && x.num == "1").unwrap().at;
        let j2 = pins.iter().find(|x| x.part == j && x.num == "2").unwrap().at;
        p.labels.push(Label { at: r1, text: "IN".into(), kind: LabelKind::Port, rot: 0 });
        // a sheet-local label on the child with the same text as one on the parent
        p.labels.push(Label { at: r2, text: "OUT".into(), kind: LabelKind::Net, rot: 0 });
        p.labels.push(Label { at: j2, text: "OUT".into(), kind: LabelKind::Net, rot: 0 });
        // flat: the port joins nothing on the parent yet, and both OUT labels are one net
        let nl = p.netlist();
        assert_eq!(nl.nets[nl.by_name("OUT").unwrap()].pins.len(), 2);
        // the symbol on the parent, its IN entry wired to J1 pin 1
        let si = p.add_sheet_symbol(child, [20.0, 20.0]).unwrap();
        assert_eq!(p.sheet_symbols[si].entries.len(), 1);
        let e = p.sheet_symbols[si].entry_at(0);
        p.wires.push(Wire { a: e, b: [e[0], j1[1]] });
        p.wires.push(Wire { a: [e[0], j1[1]], b: j1 });
        let nl = p.netlist();
        let n = nl.pin_net[&(j, "1".to_string())];
        assert_eq!(nl.pin_net.get(&(r, "1".to_string())), Some(&n), "J1.1 reaches R1.1 through the entry");
        // hierarchical now: the child's OUT is its own net
        let a = nl.pin_net.get(&(r, "2".to_string()));
        let b = nl.pin_net.get(&(j, "2".to_string()));
        assert!(a.is_none() || a != b, "sheet-local labels don't join across sheets");
        assert!(nl.by_name("OUT").is_some());
        // no loops
        assert!(p.add_sheet_symbol(0, origin(child)).is_err());
        let g = p.add_sheet("Grandchild");
        p.add_sheet_symbol(g, crate::pcb::add(origin(child), [30.0, 0.0])).unwrap();
        let s = p.symbol_of(child).unwrap();
        p.sheet_symbols.remove(s);
        assert!(p.add_sheet_symbol(child, origin(g)).is_err(), "the grandchild sits inside the child");
        assert!(p.add_sheet_symbol(g, [50.0, 0.0]).is_err(), "already has a symbol");
        assert!(p.add_sheet_symbol(child, [20.0, 20.0]).is_ok());
    }

    #[test]
    fn variants_fit_and_change_parts() {
        let mut p = crate::pcb::tests::led_board();
        p.variants.push(Variant { name: "Lite".into(), dnp: vec!["D1".into()], values: [("R1".to_string(), "1k".to_string())].into(), ..Default::default() });
        let r = p.parts.iter().position(|x| x.reference == "R1").unwrap();
        let d = p.parts.iter().position(|x| x.reference == "D1").unwrap();
        assert!(p.fitted(d));
        p.variant = "Lite".into();
        assert!(!p.fitted(d));
        assert_eq!(p.value_of(r), "1k");
        let lite = p.as_variant("Lite");
        assert!(lite.parts[d].dnp && lite.parts[r].value == "1k" && lite.variant.is_empty());
        p.set_fitted(d, true);
        assert!(p.fitted(d) && p.variants[0].dnp.is_empty());
        p.set_variant_value(r, "330");
        assert!(p.variants[0].values.is_empty(), "the base value clears the override");
        p.variant.clear();
        p.set_fitted(d, false);
        assert!(p.parts[d].dnp);
        p.variant = "Lite".into();
        p.set_fitted(d, true);
        assert_eq!(p.variants[0].fitted, vec!["D1".to_string()]);
        assert!(p.fitted(d));
    }
}
