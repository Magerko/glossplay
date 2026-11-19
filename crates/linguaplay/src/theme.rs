use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use lp_core::Ms;
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(14, 15, 18);
pub const PANEL: Color32 = Color32::from_rgb(22, 24, 29);
pub const RAISED: Color32 = Color32::from_rgb(34, 37, 45);
pub const LINE: Color32 = Color32::from_rgb(48, 52, 62);
pub const TEXT: Color32 = Color32::from_rgb(232, 234, 240);
pub const MUTED: Color32 = Color32::from_rgb(140, 147, 162);
pub const ACCENT: Color32 = Color32::from_rgb(94, 201, 168);
pub const SUB_PRIMARY: Color32 = Color32::from_rgb(255, 255, 255);
pub const SUB_SECONDARY: Color32 = Color32::from_rgb(246, 214, 128);
pub const DANGER: Color32 = Color32::from_rgb(235, 110, 110);

pub fn clock(ms: Ms) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn apply(ctx: &egui::Context, scale: f32) {
    ctx.set_pixels_per_point(ctx.native_pixels_per_point().unwrap_or(1.0) * scale.clamp(0.5, 3.0));
    let mut style = (*ctx.global_style()).clone();
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = RAISED;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_stroke = egui::Stroke::new(1.0, LINE);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.hovered.bg_fill = LINE;
    v.widgets.hovered.weak_bg_fill = LINE;
    v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.5);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.5);
    let radius = egui::CornerRadius::same(6);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// The fonts bundled with egui have no CJK glyphs, so I add whatever the system offers
/// as fallbacks. A missing file is simply skipped.
pub fn install_fonts(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            "C:/Windows/Fonts/segoeui.ttf",
            "C:/Windows/Fonts/YuGothM.ttc",
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/msyh.ttc",
            "C:/Windows/Fonts/malgun.ttf",
            "C:/Windows/Fonts/seguisym.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        ]
    };
    let mut fonts = FontDefinitions::default();
    for (i, path) in candidates.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = format!("system-{i}");
        fonts
            .font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        let family = fonts.families.entry(FontFamily::Proportional).or_default();
        // Segoe UI reads better than the bundled font, so it goes first; the rest are fallbacks.
        if path.ends_with("segoeui.ttf") {
            family.insert(0, name);
        } else {
            family.push(name);
        }
    }
    ctx.set_fonts(fonts);
}
