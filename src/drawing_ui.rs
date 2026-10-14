//! Drawing tab (placeholder until the drawing editor lands).

use egui::Ui;

use crate::app::App;

#[derive(Default)]
pub struct DrawUi {
    pub dirty: bool,
}

pub fn toolbar(_app: &mut App, _ui: &mut Ui) {}
pub fn sidebar(_app: &mut App, _ui: &mut Ui) {}
pub fn show(_app: &mut App, ui: &mut Ui) {
    ui.label("Drawings are coming next.");
}
pub fn shortcuts(_app: &mut App, _ctx: &egui::Context) {}
pub fn add_standard_views(_app: &mut App) {}
