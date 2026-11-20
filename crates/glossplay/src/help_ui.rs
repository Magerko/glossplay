//! The "How to use" window: a few steps for a first evening with the player, links to the
//! tabs of Settings they concern, and the keys, read from the key table itself.

use crate::app::App;
use crate::i18n::{Tr, fill, t, tf, tk};
use crate::keys::{self, Command};
use crate::settings_ui::SettingsTab;
use crate::theme;
use crate::ui::dialog_height;
use eframe::egui::{self, Align2, Id, RichText, vec2};

/// The keys worth knowing, grouped the way people use them.
const KEYS: [(&[Command], Tr); 15] = [
    (&[Command::PlayPause], Tr::KeyPlayPause),
    (&[Command::Back, Command::Forward], Tr::KeySeek),
    (&[Command::VolumeUp, Command::VolumeDown], Tr::KeyVolume),
    (
        &[
            Command::PreviousLine,
            Command::RepeatLine,
            Command::NextLine,
        ],
        Tr::KeyLines,
    ),
    (&[Command::AutoPause], Tr::KeyAutoPause),
    (
        &[Command::ToggleOriginal, Command::ToggleTranslation],
        Tr::KeyToggleLines,
    ),
    (&[Command::Slower, Command::Faster], Tr::KeySpeed),
    (
        &[Command::SubtitlesSmaller, Command::SubtitlesLarger],
        Tr::KeySubSize,
    ),
    (&[Command::Sidebar], Tr::KeySidebar),
    (&[Command::Fullscreen], Tr::KeyFullscreen),
    (&[Command::Mute], Tr::KeyMute),
    (&[Command::VideoInfo], Tr::KeyVideoInfo),
    (&[Command::OpenFile], Tr::KeyOpenFile),
    (&[Command::Search], Tr::KeySearch),
    (&[Command::Escape], Tr::KeyClose),
];

/// "A / S / D": the first key of each command.
fn keys_text(commands: &[Command]) -> String {
    commands
        .iter()
        .filter_map(|c| keys::keys_of(*c).into_iter().next())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn without_mark(text: &str) -> &str {
    text.trim_start_matches(['☆', '★', ' '])
}

impl App {
    pub fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = true;
        let mut go = None;
        let search = self.search_available();
        let width = (ctx.content_rect().width() - 64.0).clamp(260.0, 580.0);
        egui::Window::new(t(Tr::HelpTitle))
            .id(Id::unique("help-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(dialog_height(ctx))
                    .min_scrolled_height(dialog_height(ctx).min(560.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let menu = t(Tr::SubtitlesMenu);
                        let steps = [
                            (
                                Tr::HelpOpenTitle,
                                fill(&tk(Tr::HelpOpenText), &[&t(Tr::OpenVideo)]),
                                &[][..],
                            ),
                            (
                                Tr::HelpSubsTitle,
                                tf(Tr::HelpSubsText, &[&menu]),
                                &[SettingsTab::Languages, SettingsTab::Subtitles][..],
                            ),
                            (
                                Tr::HelpAsrTitle,
                                tf(Tr::HelpAsrText, &[&menu, &t(Tr::TranscribeSpeech)]),
                                &[SettingsTab::SpeechRecognition][..],
                            ),
                            (
                                Tr::HelpVoiceTitle,
                                tf(
                                    Tr::HelpVoiceText,
                                    &[
                                        &menu,
                                        &t(Tr::MenuVoiceOver),
                                        &t(Tr::SliderOriginal),
                                        &t(Tr::SliderVoice),
                                    ],
                                ),
                                &[SettingsTab::VoiceOver][..],
                            ),
                            (
                                Tr::HelpWordsTitle,
                                tf(
                                    Tr::HelpWordsText,
                                    &[
                                        &without_mark(t(Tr::SaveWord)),
                                        &keys_text(&[Command::Sidebar]),
                                    ],
                                ),
                                &[SettingsTab::Words][..],
                            ),
                        ];
                        for (i, (title, text, tabs)) in steps.iter().enumerate() {
                            if let Some(tab) = step(ui, i + 1, t(*title), text, tabs) {
                                go = Some(tab);
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new(t(Tr::HelpKeysTitle)).strong());
                        egui::Grid::new("help-keys")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for (commands, what) in KEYS {
                                    if commands.contains(&Command::Search) && !search {
                                        continue;
                                    }
                                    ui.label(
                                        RichText::new(keys_text(commands))
                                            .strong()
                                            .color(theme::accent()),
                                    );
                                    ui.label(t(what));
                                    ui.end_row();
                                }
                            });
                    });
            });
        if let Some(tab) = go {
            self.help_open = false;
            self.open_settings(Some(tab));
        } else if !open {
            self.help_open = false;
        }
    }
}

/// One numbered step; returns the tab of Settings whose link was clicked.
fn step(
    ui: &mut egui::Ui,
    number: usize,
    title: &str,
    text: &str,
    tabs: &[SettingsTab],
) -> Option<SettingsTab> {
    let mut go = None;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(number.to_string())
                .strong()
                .color(theme::accent()),
        );
        ui.label(RichText::new(title).strong());
    });
    ui.label(text);
    if !tabs.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for tab in tabs {
                let link = ui.link(tf(Tr::SettingsOf, &[&tab.title()]));
                crate::record::place(&format!("help_to_{}", tab.id()), link.rect);
                if link.clicked() {
                    go = Some(*tab);
                }
            }
        });
    }
    ui.add_space(8.0);
    go
}
