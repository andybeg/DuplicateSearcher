#![forbid(unsafe_code)]

mod ui;

use eframe::egui;
use std::sync::Arc;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Поиск дублей")
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([920.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Поиск дублей",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            let saved = cc.storage.and_then(|storage| storage.get_string("theme"));
            let app = ui::DuplicateApp::new(saved.as_deref());
            app.apply_look(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/DejaVuSans.ttf"
        ))),
    );
    fonts.font_data.insert(
        "dejavu-mono".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/DejaVuSansMono.ttf"
        ))),
    );
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(0, "dejavu".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        family.insert(0, "dejavu-mono".to_owned());
        family.insert(1, "dejavu".to_owned());
    }
    ctx.set_fonts(fonts);
}
