//! Stroked SVG icons rendered through egui_extras' SVG loader.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::icon_paths::ICONS;

fn table() -> &'static HashMap<&'static str, &'static [u8]> {
    static T: OnceLock<HashMap<&'static str, &'static [u8]>> = OnceLock::new();
    T.get_or_init(|| {
        ICONS
            .iter()
            .map(|(name, d)| {
                let svg = format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="#ffffff" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="{d}"/></svg>"##
                );
                (*name, &*Box::leak(svg.into_bytes().into_boxed_slice()))
            })
            .collect()
    })
}

/// An icon image `size` points square, white so it can be tinted.
pub fn image(name: &str, size: f32) -> egui::Image<'static> {
    let bytes = table().get(name).copied().unwrap_or_else(|| table()["more"]);
    egui::Image::from_bytes(format!("bytes://icon/{name}.svg"), bytes)
        .fit_to_exact_size(egui::vec2(size, size))
}
