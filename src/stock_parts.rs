//! Stock for the parts of a document: a part's own settings and material, the document units, and a
//! cache so panels can ask every frame. The sizing itself is in `stock_size` (plain geometry, so the
//! Python bindings build it too).

use crate::stock_size::{for_mesh, StockPref, StockSize};

pub fn imperial_for(pref: &StockPref, units: crate::expr::Units) -> bool {
    match pref.standard.as_str() {
        "metric" => false,
        "imperial" => true,
        _ => matches!(units, crate::expr::Units::In | crate::expr::Units::Ft),
    }
}

/// Materials that are printed or poured rather than cut from stock.
const NOT_STOCK: [&str; 5] = ["PLA", "ABS", "PETG", "Nylon", "Urethane foam"];

/// Whether a part is cut from raw stock: it has a stock setting, or a material that is not a printing plastic.
pub fn wants_stock(props: &crate::doc::PartProps) -> bool {
    props.stock.is_some() || props.material.as_deref().is_some_and(|m| !NOT_STOCK.contains(&m))
}

thread_local! {
    /// Last answers by (mesh positions address, vertex count, volume, settings), so panels can ask every frame.
    static CACHE: std::cell::RefCell<Vec<((usize, usize, u64, String), Option<StockSize>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The stock a part needs, from its settings (`None` for an open surface or an empty part).
pub fn for_part(mesh: &crate::kernel::BodyMesh, props: &crate::doc::PartProps, units: crate::expr::Units) -> Option<StockSize> {
    if mesh.surface {
        return None;
    }
    let pref = props.stock.clone().unwrap_or_default();
    let imperial = imperial_for(&pref, units);
    let key = (mesh.positions.as_ptr() as usize, mesh.positions.len(), mesh.volume.to_bits(), format!("{pref:?}{imperial}"));
    if let Some(hit) = CACHE.with(|c| c.borrow().iter().find(|e| e.0 == key).map(|e| e.1.clone())) {
        return hit;
    }
    let s = for_mesh(&mesh.positions, &pref, imperial);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 64 {
            c.remove(0);
        }
        c.push((key, s.clone()));
    });
    s
}

/// What a BOM shows for a part's stock: the size typed in, else the suggestion ("" when it needs none).
pub fn bom_text(mesh: Option<&crate::kernel::BodyMesh>, props: &crate::doc::PartProps, units: crate::expr::Units) -> String {
    if !wants_stock(props) {
        return String::new();
    }
    match props.stock.as_ref().map(|p| p.custom.trim()).filter(|c| !c.is_empty()) {
        Some(c) => c.to_string(),
        None => mesh.and_then(|m| for_part(m, props, units)).map(|s| s.label()).unwrap_or_default(),
    }
}
