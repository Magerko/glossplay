//! How subtitles are drawn over the picture: a few ready-made looks, the size multiplier
//! and the place. None of it depends on the interface theme.

use crate::i18n::Tr;
use eframe::egui::{self, Color32, Pos2, vec2};
use std::sync::Arc;

pub const SCALE_MIN: f32 = 0.6;
pub const SCALE_MAX: f32 = 2.0;
pub const SCALE_STEP: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubStyle {
    Plate,
    Outline,
    Bold,
    Yellow,
    Light,
}

/// Everything the painter needs to know about a look.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub original: Color32,
    pub translation: Color32,
    /// Behind each line; none for the looks that rely on the outline.
    pub plate: Option<Color32>,
    pub outline: Option<(Color32, f32)>,
    pub shadow: bool,
    /// Drawn twice, a little apart: egui has no bold face to ask for.
    pub bold: bool,
    pub size: f32,
    /// The hovered or opened word, and the underline of saved words.
    pub hover: Color32,
    pub saved: Color32,
}

impl SubStyle {
    pub const ALL: [SubStyle; 5] = [
        SubStyle::Plate,
        SubStyle::Outline,
        SubStyle::Bold,
        SubStyle::Yellow,
        SubStyle::Light,
    ];

    pub fn id(self) -> &'static str {
        match self {
            SubStyle::Plate => "plate",
            SubStyle::Outline => "outline",
            SubStyle::Bold => "bold",
            SubStyle::Yellow => "yellow",
            SubStyle::Light => "light",
        }
    }

    /// An unknown name gives the default look, so a config from a newer version still works.
    pub fn from_id(id: &str) -> SubStyle {
        SubStyle::ALL
            .into_iter()
            .find(|s| s.id() == id.trim().to_ascii_lowercase())
            .unwrap_or(SubStyle::Plate)
    }

    pub fn name(self) -> Tr {
        match self {
            SubStyle::Plate => Tr::SubStylePlate,
            SubStyle::Outline => Tr::SubStyleOutline,
            SubStyle::Bold => Tr::SubStyleBold,
            SubStyle::Yellow => Tr::SubStyleYellow,
            SubStyle::Light => Tr::SubStyleLight,
        }
    }

    pub fn look(self) -> Look {
        let black = |a| Color32::from_black_alpha(a);
        let teal = Color32::from_rgb(94, 201, 168);
        match self {
            SubStyle::Plate => Look {
                original: Color32::from_rgb(255, 255, 255),
                translation: Color32::from_rgb(246, 214, 128),
                plate: Some(black(140)),
                outline: Some((black(230), 1.6)),
                shadow: false,
                bold: false,
                size: 1.0,
                hover: teal.gamma_multiply(0.45),
                saved: teal,
            },
            SubStyle::Outline => Look {
                original: Color32::from_rgb(255, 255, 255),
                translation: Color32::from_rgb(226, 226, 210),
                plate: None,
                outline: Some((black(255), 2.0)),
                shadow: true,
                bold: false,
                size: 1.0,
                hover: teal.gamma_multiply(0.5),
                saved: teal,
            },
            SubStyle::Bold => Look {
                original: Color32::from_rgb(255, 255, 255),
                translation: Color32::from_rgb(255, 236, 170),
                plate: None,
                outline: Some((black(255), 2.6)),
                shadow: true,
                bold: true,
                size: 1.3,
                hover: teal.gamma_multiply(0.5),
                saved: teal,
            },
            SubStyle::Yellow => Look {
                original: Color32::from_rgb(255, 226, 0),
                translation: Color32::from_rgb(238, 238, 238),
                plate: None,
                outline: Some((black(255), 2.2)),
                shadow: true,
                bold: false,
                size: 1.0,
                hover: Color32::from_rgb(94, 201, 168).gamma_multiply(0.55),
                saved: teal,
            },
            SubStyle::Light => Look {
                original: Color32::from_rgb(20, 22, 28),
                translation: Color32::from_rgb(72, 78, 92),
                plate: Some(Color32::from_rgba_unmultiplied(245, 245, 242, 236)),
                outline: None,
                shadow: false,
                bold: false,
                size: 1.0,
                hover: Color32::from_rgb(0, 150, 120).gamma_multiply(0.35),
                saved: Color32::from_rgb(0, 130, 104),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Bottom,
    Top,
}

impl Position {
    pub fn from_id(id: &str) -> Position {
        if id.trim().eq_ignore_ascii_case("top") {
            Position::Top
        } else {
            Position::Bottom
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Position::Bottom => "bottom",
            Position::Top => "top",
        }
    }

    pub fn name(self) -> Tr {
        match self {
            Position::Bottom => Tr::SubPosBottom,
            Position::Top => Tr::SubPosTop,
        }
    }
}

/// The multiplier a config holds, made safe: a hand-edited or missing value must not make
/// the subtitles vanish or fill the screen.
pub fn clamp_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(SCALE_MIN, SCALE_MAX)
    } else {
        1.0
    }
}

/// One step up or down, on a grid of tenths so repeated presses do not drift.
pub fn step_scale(scale: f32, up: bool) -> f32 {
    let tenths = (clamp_scale(scale) / SCALE_STEP).round();
    let tenths = if up { tenths + 1.0 } else { tenths - 1.0 };
    clamp_scale((tenths * SCALE_STEP * 100.0).round() / 100.0)
}

/// Subtitle text with the outline, shadow and weight of `look`.
pub fn paint(
    painter: &egui::Painter,
    at: Pos2,
    galley: Arc<egui::Galley>,
    color: Color32,
    look: &Look,
    scale: f32,
) {
    if look.shadow {
        let d = 2.2 * scale;
        painter.galley_with_override_text_color(
            at + vec2(d, d),
            galley.clone(),
            Color32::from_black_alpha(150),
        );
    }
    if let Some((outline, width)) = look.outline {
        let d = (width * scale).max(1.0);
        for (dx, dy) in [
            (-d, 0.0),
            (d, 0.0),
            (0.0, -d),
            (0.0, d),
            (-d, -d),
            (d, -d),
            (-d, d),
            (d, d),
        ] {
            painter.galley_with_override_text_color(at + vec2(dx, dy), galley.clone(), outline);
        }
    }
    if look.bold {
        let d = (0.04 * galley.size().y).max(0.8);
        painter.galley(at + vec2(d, 0.0), galley.clone(), color);
        painter.galley(at - vec2(d, 0.0), galley.clone(), color);
    }
    painter.galley(at, galley, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_style_and_position_mean_the_defaults() {
        assert_eq!(SubStyle::from_id("yellow"), SubStyle::Yellow);
        assert_eq!(SubStyle::from_id(" Outline "), SubStyle::Outline);
        assert_eq!(SubStyle::from_id("neon-from-the-future"), SubStyle::Plate);
        assert_eq!(SubStyle::from_id(""), SubStyle::Plate);
        assert_eq!(Position::from_id("top"), Position::Top);
        assert_eq!(Position::from_id("sideways"), Position::Bottom);
        for s in SubStyle::ALL {
            assert_eq!(SubStyle::from_id(s.id()), s);
        }
    }

    #[test]
    fn the_default_look_is_the_old_one() {
        let l = SubStyle::Plate.look();
        assert_eq!(l.original, Color32::from_rgb(255, 255, 255));
        assert_eq!(l.translation, Color32::from_rgb(246, 214, 128));
        assert_eq!(l.plate, Some(Color32::from_black_alpha(140)));
        assert_eq!(l.outline, Some((Color32::from_black_alpha(230), 1.6)));
        assert_eq!(l.size, 1.0);
    }

    #[test]
    fn size_steps_stay_on_the_grid_and_inside_the_limits() {
        assert_eq!(step_scale(1.0, true), 1.1);
        assert_eq!(step_scale(1.0, false), 0.9);
        let mut s = 1.0;
        for _ in 0..30 {
            s = step_scale(s, true);
        }
        assert_eq!(s, SCALE_MAX);
        for _ in 0..30 {
            s = step_scale(s, false);
        }
        assert_eq!(s, SCALE_MIN);
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        assert_eq!(clamp_scale(0.0), SCALE_MIN);
        assert_eq!(clamp_scale(9.0), SCALE_MAX);
    }
}
