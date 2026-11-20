use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use gp_core::Ms;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The colours of the interface around the video. The video itself, the subtitles over it
/// and the control bar drawn on it do not follow the theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub line: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub danger: Color32,
    /// Notes that should stand out a little from the muted text.
    pub hint: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(14, 15, 18),
    panel: Color32::from_rgb(22, 24, 29),
    raised: Color32::from_rgb(34, 37, 45),
    line: Color32::from_rgb(48, 52, 62),
    text: Color32::from_rgb(232, 234, 240),
    muted: Color32::from_rgb(140, 147, 162),
    accent: Color32::from_rgb(94, 201, 168),
    danger: Color32::from_rgb(235, 110, 110),
    hint: Color32::from_rgb(246, 214, 128),
};

// Text colours are picked for WCAG AA (4.5:1) on `panel` and on `raised`: the accent is a
// darker teal than in the dark theme and the hint an amber brown, after the icon's colours.
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(236, 239, 243),
    panel: Color32::from_rgb(248, 249, 251),
    raised: Color32::from_rgb(232, 236, 241),
    line: Color32::from_rgb(200, 206, 216),
    text: Color32::from_rgb(24, 28, 36),
    muted: Color32::from_rgb(88, 96, 112),
    accent: Color32::from_rgb(0, 112, 90),
    danger: Color32::from_rgb(176, 40, 40),
    hint: Color32::from_rgb(128, 76, 0),
};

static LIGHT_ON: AtomicBool = AtomicBool::new(false);

pub fn palette() -> Palette {
    if LIGHT_ON.load(Ordering::Relaxed) {
        LIGHT
    } else {
        DARK
    }
}

pub fn bg() -> Color32 {
    palette().bg
}
pub fn panel() -> Color32 {
    palette().panel
}
pub fn raised() -> Color32 {
    palette().raised
}
pub fn text() -> Color32 {
    palette().text
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn danger() -> Color32 {
    palette().danger
}
pub fn hint() -> Color32 {
    palette().hint
}

/// Whether the interface is light: `setting` is "dark", "light" or "system", and
/// `system_dark` what the system reports, if it does. Anything unknown means dark.
pub fn is_light(setting: &str, system_dark: Option<bool>) -> bool {
    match setting.trim().to_ascii_lowercase().as_str() {
        "light" => true,
        "system" => system_dark == Some(false),
        _ => false,
    }
}

pub fn clock(ms: Ms) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn apply(ctx: &egui::Context, scale: f32, theme: &str) {
    ctx.set_pixels_per_point(ctx.native_pixels_per_point().unwrap_or(1.0) * scale.clamp(0.5, 3.0));
    apply_colors(ctx, theme);
}

/// The colours and shapes of the widgets; the size of the interface is left alone.
pub fn apply_colors(ctx: &egui::Context, theme: &str) {
    let system_dark = ctx.system_theme().map(|t| t == egui::Theme::Dark);
    let light = is_light(theme, system_dark);
    LIGHT_ON.store(light, Ordering::Relaxed);
    ctx.set_style_of(egui::Theme::Dark, style(light));
    ctx.set_style_of(egui::Theme::Light, style(light));
    ctx.set_theme(if light {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    });
}

/// The style of widgets in a palette; the control bar uses the dark one over the video.
pub fn style(light: bool) -> egui::Style {
    let p = if light { LIGHT } else { DARK };
    let mut style = egui::Style::default();
    let v = &mut style.visuals;
    *v = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = if light { Color32::WHITE } else { p.bg };
    v.faint_bg_color = p.raised;
    v.override_text_color = Some(p.text);
    v.selection.bg_fill = p.accent.gamma_multiply(if light { 0.25 } else { 0.35 });
    v.selection.stroke = egui::Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.window_stroke = egui::Stroke::new(1.0, p.line);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.line);
    v.widgets.inactive.bg_fill = p.raised;
    v.widgets.inactive.weak_bg_fill = p.raised;
    v.widgets.hovered.bg_fill = p.line;
    v.widgets.hovered.weak_bg_fill = p.line;
    v.widgets.active.bg_fill = p.accent.gamma_multiply(if light { 0.35 } else { 0.5 });
    v.widgets.active.weak_bg_fill = p.accent.gamma_multiply(if light { 0.35 } else { 0.5 });
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
    style
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

    #[test]
    fn the_theme_setting_is_read_forgivingly() {
        assert!(!is_light("dark", Some(false)));
        assert!(is_light("light", Some(true)));
        assert!(is_light(" Light ", None));
        assert!(is_light("system", Some(false)));
        assert!(!is_light("system", Some(true)));
        assert!(
            !is_light("system", None),
            "no answer from the system means dark"
        );
        assert!(!is_light("", None));
        assert!(!is_light("sepia", Some(false)));
    }

    fn luminance(c: Color32) -> f64 {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_colours_reach_wcag_aa_in_both_palettes() {
        for (name, p) in [("dark", DARK), ("light", LIGHT)] {
            for ground in [p.panel, p.raised, p.bg] {
                for (what, fg) in [
                    ("text", p.text),
                    ("muted", p.muted),
                    ("accent", p.accent),
                    ("danger", p.danger),
                    ("hint", p.hint),
                ] {
                    let ratio = contrast(fg, ground);
                    assert!(ratio >= 4.5, "{name} {what} on {ground:?}: {ratio:.2}");
                }
            }
        }
    }
}
