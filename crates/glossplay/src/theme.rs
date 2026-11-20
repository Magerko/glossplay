use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use gp_core::Ms;
use std::collections::HashMap;
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(14, 15, 18);
pub const PANEL: Color32 = Color32::from_rgb(22, 24, 29);
pub const RAISED: Color32 = Color32::from_rgb(34, 37, 45);
pub const LINE: Color32 = Color32::from_rgb(48, 52, 62);
pub const TEXT: Color32 = Color32::from_rgb(232, 234, 240);
pub const MUTED: Color32 = Color32::from_rgb(140, 147, 162);
pub const ACCENT: Color32 = Color32::from_rgb(94, 201, 168);
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

/// One face of a system font file. `index` picks a face inside a .ttc collection.
struct Face(&'static str, u32);

/// System fonts per script. One CJK font has to draw a whole line: when a Japanese font
/// covers only some of the Chinese characters and another font the rest, the two have
/// different ascents and the glyphs end up at different sizes and heights.
struct Scripts {
    latin: &'static [Face],
    zh: &'static [Face],
    ja: &'static [Face],
    ko: &'static [Face],
    symbols: &'static [Face],
}

fn system_fonts() -> Scripts {
    if cfg!(windows) {
        Scripts {
            latin: &[Face("C:/Windows/Fonts/segoeui.ttf", 0)],
            zh: &[Face("C:/Windows/Fonts/msyh.ttc", 0)],
            ja: &[
                Face("C:/Windows/Fonts/YuGothM.ttc", 0),
                Face("C:/Windows/Fonts/meiryo.ttc", 0),
            ],
            ko: &[Face("C:/Windows/Fonts/malgun.ttf", 0)],
            symbols: &[Face("C:/Windows/Fonts/seguisym.ttf", 0)],
        }
    } else if cfg!(target_os = "macos") {
        Scripts {
            latin: &[],
            zh: &[
                Face("/System/Library/Fonts/PingFang.ttc", 0),
                Face("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
            ],
            ja: &[Face("/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc", 0)],
            ko: &[Face("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0)],
            symbols: &[],
        }
    } else {
        // Noto Sans CJK ships as one collection ordered JP, KR, SC, TC, HK.
        Scripts {
            latin: &[],
            zh: &[
                Face("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
                Face("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
                Face(
                    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                    2,
                ),
                Face("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 2),
            ],
            ja: &[
                Face("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
                Face("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
                Face(
                    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                    0,
                ),
                Face("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 0),
            ],
            ko: &[
                Face("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1),
                Face("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 1),
                Face(
                    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                    1,
                ),
                Face("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 1),
            ],
            symbols: &[],
        }
    }
}

/// The fonts bundled with egui have no CJK glyphs, so I add whatever the system offers
/// as fallbacks. A missing file is simply skipped.
pub fn install_fonts(ctx: &egui::Context) {
    let scripts = system_fonts();
    let mut fonts = FontDefinitions::default();
    let mut files: HashMap<&str, Arc<Vec<u8>>> = HashMap::new();
    let mut load_all = |fonts: &mut FontDefinitions, faces: &'static [Face]| -> Vec<String> {
        let mut names = Vec::new();
        for face in faces {
            let name = format!("system:{}:{}", face.0, face.1);
            if !fonts.font_data.contains_key(&name) {
                let bytes = match files.get(face.0) {
                    Some(b) => b.clone(),
                    None => {
                        let Ok(read) = std::fs::read(face.0) else {
                            continue;
                        };
                        let b = Arc::new(read);
                        files.insert(face.0, b.clone());
                        b
                    }
                };
                fonts
                    .font_data
                    .insert(name.clone(), Arc::new(FontData::from_blob(bytes, face.1)));
            }
            names.push(name);
        }
        names
    };
    let latin = load_all(&mut fonts, scripts.latin);
    let zh = load_all(&mut fonts, scripts.zh);
    let ja = load_all(&mut fonts, scripts.ja);
    let ko = load_all(&mut fonts, scripts.ko);
    let symbols = load_all(&mut fonts, scripts.symbols);

    // The general chain keeps the old order: Japanese first, since kana-only text is the
    // most common reason to need a fallback at all.
    let general: Vec<String> = [&ja, &zh, &ko, &symbols]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    // Segoe UI reads better than the bundled font, so it goes first; the rest are fallbacks.
    for (i, name) in latin.iter().enumerate() {
        proportional.insert(i, name.clone());
    }
    proportional.extend(general);
    let base = proportional.clone();

    // A language family lists its own font first and then the same chain as everywhere
    // else, so a missing font makes it identical to the general family.
    for (family, own) in [(SUBS_ZH, &zh), (SUBS_JA, &ja), (SUBS_KO, &ko)] {
        let mut chain: Vec<String> = own.clone();
        chain.extend(base.iter().filter(|n| !own.contains(n)).cloned());
        fonts
            .families
            .insert(FontFamily::Name(family.into()), chain);
    }
    ctx.set_fonts(fonts);
}

const SUBS_ZH: &str = "subs-zh";
const SUBS_JA: &str = "subs-ja";
const SUBS_KO: &str = "subs-ko";

/// The font family for subtitle text: by the track language when it is known, otherwise by
/// what the text contains (kana means Japanese, Hangul Korean, other ideographs Chinese).
pub fn subs_family(lang: Option<&str>, text: &str) -> FontFamily {
    let code = lang
        .map(|l| crate::session::normalize_lang(l.trim()))
        .filter(|l| !l.is_empty() && l != "und" && l != "auto");
    let name = match code.as_deref() {
        Some("zh" | "yue") => SUBS_ZH,
        Some("ja") => SUBS_JA,
        Some("ko") => SUBS_KO,
        Some(_) => return FontFamily::Proportional,
        None => match detect_script(text) {
            Some(Script::Ja) => SUBS_JA,
            Some(Script::Ko) => SUBS_KO,
            Some(Script::Zh) => SUBS_ZH,
            None => return FontFamily::Proportional,
        },
    };
    FontFamily::Name(name.into())
}

#[derive(Debug, PartialEq)]
enum Script {
    Ja,
    Ko,
    Zh,
}

fn detect_script(text: &str) -> Option<Script> {
    let mut han = false;
    let mut hangul = false;
    for c in text.chars() {
        match c {
            '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' => return Some(Script::Ja),
            '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}' => hangul = true,
            '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' => {
                han = true
            }
            _ => {}
        }
    }
    if hangul {
        Some(Script::Ko)
    } else if han {
        Some(Script::Zh)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(f: FontFamily) -> String {
        match f {
            FontFamily::Name(n) => n.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn family_follows_the_language_tag() {
        assert_eq!(name(subs_family(Some("zh"), "x")), "subs-zh");
        assert_eq!(name(subs_family(Some("zh-CN"), "x")), "subs-zh");
        assert_eq!(name(subs_family(Some("jpn"), "x")), "subs-ja");
        assert_eq!(name(subs_family(Some("ko"), "x")), "subs-ko");
        assert_eq!(name(subs_family(Some("en"), "漢字")), "Proportional");
    }

    #[test]
    fn family_is_detected_from_the_text() {
        assert_eq!(name(subs_family(None, "你好，世界")), "subs-zh");
        assert_eq!(name(subs_family(None, "今日は天気です")), "subs-ja");
        assert_eq!(name(subs_family(Some("und"), "안녕하세요")), "subs-ko");
        assert_eq!(name(subs_family(None, "plain text")), "Proportional");
    }
}
