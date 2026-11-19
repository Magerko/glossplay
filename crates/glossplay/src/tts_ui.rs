//! The voice-over on screen: its install dialog, its part of the Settings window, and turning
//! it on and off.

use crate::app::App;
use crate::components::{self, Event, InstallError, Progress};
use crate::dub::{Dub, DubConfig, Source};
use crate::jobs::Msg;
use crate::setup_text as text;
use crate::theme::{ACCENT, DANGER, MUTED, SUB_SECONDARY};
use crate::tts_setup::{self, ENGINE_AVAILABLE, Engine, TtsInstall};
use eframe::egui::{self, Align, Align2, RichText, vec2};
use gp_core::config::Config;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DIALOG_WIDTH: f32 = 560.0;
/// The name the voice engine's server runs under, for stopping it before files change.
pub const SERVER_NAME: &str = "audiocpp_server";

enum Outcome {
    Ready { enabled: bool },
    Failed(String),
    Cancelled,
}

#[derive(Default)]
pub struct TtsSetup {
    pub dialog_open: bool,
    /// Turn the voice-over on as soon as the install is done.
    enable_after: bool,
    /// The licence box of the dialog is ticked.
    pub agreed: bool,
    cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<Progress>,
    outcome: Option<Outcome>,
    pub installed: Option<TtsInstall>,
    pub scroll_to: bool,
}

impl TtsSetup {
    pub fn new() -> TtsSetup {
        TtsSetup {
            installed: tts_setup::installed(),
            ..Default::default()
        }
    }

    pub fn running(&self) -> bool {
        self.cancel.is_some()
    }
}

pub enum Action {
    OpenDialog,
    Remove,
    Cancel,
}

impl App {
    /// What the voice-over would run on: a server of the user's own, or the installed engine.
    fn dub_config(&self) -> Result<DubConfig, String> {
        let cfg = &self.cfg.tts;
        let source = if !cfg.base_url.trim().is_empty() {
            Source::Remote {
                base_url: cfg.base_url.clone(),
                model: cfg.model.clone(),
            }
        } else {
            match tts_setup::installed() {
                Some(i) if i.server.is_some() => Source::Installed(i),
                Some(_) => return Err(text::TTS_NO_ENGINE_HERE.into()),
                None => return Err(text::TTS_NOT_SET_UP.into()),
            }
        };
        let data = crate::paths::data_dir();
        Ok(DubConfig {
            source,
            voice: cfg.voice.clone(),
            lang: self.cfg.general.native_lang.clone(),
            voice_volume: cfg.voice_volume,
            mpv_path: self.cfg.player.mpv_path.clone(),
            silent: self.quiet,
            cache_dir: data.join("tts-cache"),
            work_dir: data.join("voice"),
            save_dir: self.dub_save.clone(),
        })
    }

    pub fn dub_ready(&self) -> bool {
        self.dub_config().is_ok()
    }

    /// Turns the voice-over on or off and remembers the choice. Turning it on without an
    /// engine opens the install dialog instead.
    pub fn set_dub(&mut self, on: bool) {
        if !on {
            self.dub = None;
            self.cfg.tts.enabled = false;
            self.apply_volume();
            self.save_dub_choice();
            return;
        }
        if self.dub.is_some() {
            return;
        }
        let cfg = match self.dub_config() {
            Ok(c) => c,
            Err(_) if self.cfg.tts.base_url.trim().is_empty() => {
                return self.open_tts_setup(true);
            }
            Err(e) => return self.toast(e),
        };
        match Dub::start(cfg, self.player.mpv.clone(), self.jobs.context()) {
            Ok(d) => {
                self.dub = Some(d);
                self.cfg.tts.enabled = true;
                self.apply_volume();
                self.save_dub_choice();
                let no_translation = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.secondary == crate::session::Secondary::Off);
                if no_translation {
                    self.toast(text::TTS_NEEDS_TRANSLATION);
                }
            }
            Err(e) => self.toast(e),
        }
    }

    fn save_dub_choice(&mut self) {
        // A self-test runs on a scratch config and must leave it as the test set it up.
        if self.quiet {
            return;
        }
        if let Err(e) = crate::paths::save_config(&self.cfg) {
            self.toast(format!("Could not save settings: {e}"));
        }
    }

    /// The original sound is turned down while the voice-over plays.
    pub fn apply_volume(&self) {
        let v = gp_core::dub_plan::original_volume(
            self.volume,
            self.dub.is_some(),
            self.cfg.tts.original_volume,
        );
        self.player.mpv.set_f64("volume", v);
        if let Some(d) = &self.dub {
            d.set_voice_volume(self.cfg.tts.voice_volume);
        }
    }

    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 150.0);
        self.apply_volume();
    }

    /// Feeds the voice-over the lines around the playhead; called once a frame.
    pub fn update_dub(&mut self) {
        let Some(d) = &self.dub else { return };
        let pos = self.snap.pos;
        let units = self.session.as_ref().map_or(Vec::new(), |s| {
            gp_core::dub_plan::units(&s.dub_lines(pos - 3_000, pos + crate::dub::AHEAD_MS))
        });
        d.set_units(units);
        if let Some(alert) = d.take_alert() {
            self.toast(alert);
        }
    }

    pub fn open_tts_setup(&mut self, enable_after: bool) {
        let s = &mut self.tts_setup;
        s.dialog_open = true;
        s.enable_after |= enable_after;
        if !s.running() {
            s.outcome = None;
        }
        s.installed = tts_setup::installed();
        let _ = crate::asr_setup::hardware();
    }

    pub fn install_tts(&mut self, engine: Engine) {
        if self.tts_setup.running() || components::is_installing(tts_setup::ID) {
            return;
        }
        // A running engine keeps its files open, and Windows will not replace them.
        let was_on = self.dub.take().is_some();
        gp_media::tools::stop_named(SERVER_NAME);
        if was_on {
            self.apply_volume();
            self.tts_setup.enable_after = true;
        }
        let component = tts_setup::component(engine);
        let cancel = Arc::new(AtomicBool::new(false));
        self.tts_setup.cancel = Some(cancel.clone());
        self.tts_setup.progress = Some(Progress::default());
        self.tts_setup.outcome = None;
        self.jobs.spawn_reporting(move |send| {
            let mut last: Option<(Instant, Option<components::Stage>, String)> = None;
            let result = components::install(&component, &cancel, &mut |p| {
                let due = last.as_ref().is_none_or(|(at, stage, file)| {
                    at.elapsed() >= Duration::from_millis(100)
                        || *stage != p.stage
                        || *file != p.file
                });
                if due {
                    last = Some((Instant::now(), p.stage, p.file.clone()));
                    send(Msg::Component {
                        id: tts_setup::ID.into(),
                        event: Event::Progress(p.clone()),
                    });
                }
            });
            Msg::Component {
                id: tts_setup::ID.into(),
                event: Event::Done(result),
            }
        });
    }

    fn remove_tts(&mut self) {
        if self.dub.take().is_some() {
            self.apply_volume();
        }
        gp_media::tools::stop_named(SERVER_NAME);
        match components::remove(tts_setup::ID) {
            Ok(()) => self.toast(text::TTS_TOAST_REMOVED),
            Err(e) => self.toast(e),
        }
        self.tts_setup.installed = tts_setup::installed();
        self.tts_setup.outcome = None;
    }

    pub fn on_tts_event(&mut self, event: Event) {
        let result = match event {
            Event::Progress(p) => {
                self.tts_setup.progress = Some(p);
                return;
            }
            Event::Done(result) => result,
        };
        let s = &mut self.tts_setup;
        s.cancel = None;
        s.progress = None;
        s.installed = tts_setup::installed();
        match result {
            Ok(_) => {
                let enable = std::mem::take(&mut s.enable_after);
                s.outcome = Some(Outcome::Ready { enabled: enable });
                self.toast(text::TTS_TOAST_READY);
                if enable {
                    self.set_dub(true);
                }
            }
            Err(InstallError::Cancelled) => s.outcome = Some(Outcome::Cancelled),
            Err(InstallError::Failed(e)) => {
                s.outcome = Some(Outcome::Failed(e.clone()));
                if !s.dialog_open {
                    self.toast(e);
                }
            }
        }
    }

    pub fn tts_settings_action(&mut self, action: Action, draft: &Config) {
        match action {
            Action::OpenDialog => {
                self.cfg.tts.install_engine = draft.tts.install_engine.clone();
                self.open_tts_setup(false);
            }
            Action::Remove => self.remove_tts(),
            Action::Cancel => self.cancel_tts_install(),
        }
    }

    fn cancel_tts_install(&mut self) {
        if let Some(c) = &self.tts_setup.cancel {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn tts_setup_dialog(&mut self, ctx: &egui::Context) {
        if !self.tts_setup.dialog_open {
            return;
        }
        let mut engine = self.cfg.tts.install_engine.clone();
        let mut agreed = self.tts_setup.agreed;
        let (mut open, mut install, mut cancel, mut close) = (true, None, false, false);
        let s = &self.tts_setup;
        egui::Window::new(text::TTS_DIALOG_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(DIALOG_WIDTH);
                ui.label(text::TTS_DIALOG_INTRO);
                ui.add_space(8.0);
                hardware_line(ui);
                ui.add_enabled_ui(!s.running(), |ui| choice(ui, "tts-dialog", &mut engine));
                let e = tts_setup::resolve(&engine);
                ui.add_space(6.0);
                plan(ui, e);
                warnings(ui, e);
                if let Some(i) = &s.installed {
                    ui.label(RichText::new(i.status()).color(MUTED));
                }
                ui.add_space(6.0);
                licences(ui);
                ui.add_space(6.0);
                ui.add_enabled_ui(!s.running(), |ui| {
                    ui.checkbox(&mut agreed, RichText::new(text::TTS_AGREE).strong());
                });
                ui.add_space(8.0);
                if let Some(p) = &s.progress {
                    progress(ui, p);
                    ui.add_space(6.0);
                    if ui.button(text::CANCEL).clicked() {
                        cancel = true;
                    }
                    return;
                }
                match &s.outcome {
                    Some(Outcome::Ready { enabled }) => {
                        ui.label(RichText::new(text::TTS_READY).color(ACCENT).strong());
                        ui.label(if *enabled {
                            text::TTS_READY_ON
                        } else {
                            text::TTS_READY_HOW
                        });
                        ui.add_space(6.0);
                        if ui.button(text::CLOSE).clicked() {
                            close = true;
                        }
                        return;
                    }
                    Some(Outcome::Failed(e)) => {
                        ui.label(RichText::new(e).color(DANGER));
                    }
                    Some(Outcome::Cancelled) => {
                        ui.label(RichText::new(text::CANCELLED).color(MUTED));
                    }
                    None => {}
                }
                ui.horizontal(|ui| {
                    let label = match s.outcome {
                        Some(Outcome::Failed(_)) => text::TRY_AGAIN,
                        _ => text::INSTALL,
                    };
                    let button = egui::Button::new(RichText::new(label).strong());
                    if ui.add_enabled(agreed, button).clicked() {
                        install = Some(e);
                    }
                    if ui.button(text::CANCEL).clicked() {
                        close = true;
                    }
                });
            });
        if s.running() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.cfg.tts.install_engine = engine;
        self.tts_setup.agreed = agreed;
        if cancel {
            self.cancel_tts_install();
        }
        if let Some(e) = install.filter(|_| agreed) {
            self.install_tts(e);
        }
        if close || !open {
            self.tts_setup.dialog_open = false;
            if !self.tts_setup.running() {
                self.tts_setup.enable_after = false;
            }
        }
    }
}

/// The Voice-over part of the Settings window.
pub fn settings_section(ui: &mut egui::Ui, draft: &mut Config, s: &TtsSetup) -> Option<Action> {
    if s.scroll_to {
        let at = egui::Rect::from_min_size(ui.cursor().min - vec2(0.0, 32.0), vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(Align::TOP));
    }
    let mut action = None;
    ui.checkbox(&mut draft.tts.enabled, text::TTS_ENABLE);
    let status = match &s.installed {
        Some(i) => i.status(),
        None => text::NOT_INSTALLED_STATUS.to_owned(),
    };
    ui.label(status);
    if let Some(p) = &s.progress {
        progress(ui, p);
    }
    hardware_line(ui);
    ui.add_enabled_ui(!s.running(), |ui| {
        choice(ui, "tts-settings", &mut draft.tts.install_engine);
    });
    let size = tts_setup::component(tts_setup::resolve(&draft.tts.install_engine)).download_size();
    ui.label(
        RichText::new(format!("{}: {}", text::ROW_TOTAL, text::size(size)))
            .color(MUTED)
            .small(),
    );
    ui.horizontal(|ui| {
        if s.running() {
            if ui.button(text::CANCEL).clicked() {
                action = Some(Action::Cancel);
            }
            return;
        }
        let label = if s.installed.is_some() {
            text::REINSTALL
        } else {
            text::INSTALL
        };
        if ui.button(label).clicked() {
            action = Some(Action::OpenDialog);
        }
        if s.installed.is_some() && ui.button(text::REMOVE).clicked() {
            action = Some(Action::Remove);
        }
    });
    ui.label(RichText::new(text::tts_language(&draft.general.native_lang)).color(MUTED));
    egui::Grid::new("tts-volumes")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label(text::ROW_ORIGINAL_VOLUME);
            ui.add(egui::Slider::new(&mut draft.tts.original_volume, 0.0..=150.0).integer());
            ui.end_row();
            ui.label(text::ROW_VOICE_VOLUME);
            ui.add(egui::Slider::new(&mut draft.tts.voice_volume, 0.0..=150.0).integer());
            ui.end_row();
        });
    ui.label(RichText::new(text::TTS_ADVANCED).color(MUTED));
    egui::Grid::new("tts-own")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for (label, hint, value) in [
                (text::ROW_SERVER, text::HINT_SERVER, &mut draft.tts.base_url),
                (
                    text::ROW_TTS_MODEL,
                    text::HINT_TTS_MODEL,
                    &mut draft.tts.model,
                ),
                (text::ROW_VOICE, text::HINT_VOICE, &mut draft.tts.voice),
            ] {
                ui.label(label);
                ui.add(
                    egui::TextEdit::singleline(value)
                        .hint_text(hint)
                        .desired_width(300.0),
                );
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    action
}

fn choice(ui: &mut egui::Ui, salt: &str, engine: &mut String) {
    ui.horizontal(|ui| {
        ui.label(text::ROW_ENGINE);
        if !ENGINE_AVAILABLE {
            ui.label(text::ENGINE_OWN);
            return;
        }
        let current = tts_setup::resolve(engine);
        egui::ComboBox::from_id_salt((salt, "tts-engine"))
            .width(360.0)
            .selected_text(current.choice_name())
            .show_ui(ui, |ui| {
                for e in [Engine::Vulkan, Engine::Nvidia] {
                    if ui.selectable_label(current == e, e.choice_name()).clicked() {
                        *engine = e.key().to_owned();
                    }
                }
            });
    });
}

fn hardware_line(ui: &mut egui::Ui) {
    let line = match crate::asr_setup::hardware() {
        Some(hw) => match &hw.gpu {
            Some(g) => text::gpu_line(&g.name, g.vram_mb),
            None => text::NO_NVIDIA.to_owned(),
        },
        None => text::CHECKING_HARDWARE.to_owned(),
    };
    ui.label(RichText::new(line).color(MUTED));
}

fn plan(ui: &mut egui::Ui, engine: Engine) {
    let c = tts_setup::component(engine);
    egui::Grid::new("tts-plan")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for f in &c.files {
                ui.label(&f.label);
                ui.label(text::size(f.size));
                ui.end_row();
            }
            ui.label(RichText::new(text::ROW_TOTAL).strong());
            ui.label(RichText::new(text::size(c.download_size())).strong());
            ui.end_row();
        });
    let root = components::root();
    ui.label(
        RichText::new(text::space_line(
            c.installed_size(),
            components::space_still_needed(&c),
            components::free_space(&root),
        ))
        .color(MUTED),
    );
    ui.label(
        RichText::new(format!(
            "{}: {}",
            text::ROW_PLACE,
            components::dir(&c.id).display()
        ))
        .color(MUTED)
        .small(),
    );
    ui.label(RichText::new(text::TTS_SOURCES).color(MUTED).small());
}

fn warnings(ui: &mut egui::Ui, engine: Engine) {
    let mut warn = |t: &str| {
        ui.label(RichText::new(t).color(SUB_SECONDARY));
    };
    if !ENGINE_AVAILABLE {
        warn(text::TTS_NO_ENGINE_HERE);
        return;
    }
    if !tts_setup::vulkan_loader_present() {
        warn(text::TTS_NO_VULKAN);
    } else {
        warn(text::TTS_NEEDS_GPU);
    }
    let no_nvidia = crate::asr_setup::hardware().is_some_and(|hw| hw.gpu.is_none());
    if engine == Engine::Nvidia && no_nvidia {
        warn(text::TTS_NVIDIA_WITHOUT_GPU);
    }
}

fn licences(ui: &mut egui::Ui) {
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(6)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text::TTS_LICENCE_NC).strong().color(DANGER));
            ui.label(RichText::new(text::TTS_NO_IMPERSONATION).strong());
            ui.label(text::TTS_LICENCE_BOSON);
            ui.label(RichText::new(text::TTS_LICENCE_ENGINE).color(MUTED).small());
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to(text::TTS_LINK_CC, text::TTS_URL_CC);
                ui.label("·");
                ui.hyperlink_to(text::TTS_LINK_BOSON, text::TTS_URL_BOSON);
                ui.label("·");
                ui.hyperlink_to(text::TTS_LINK_MODEL, text::TTS_URL_MODEL);
            });
        });
}

fn progress(ui: &mut egui::Ui, p: &Progress) {
    let stage = p.stage.map(|s| text::stage(s, &p.file)).unwrap_or_default();
    ui.add(
        egui::ProgressBar::new(p.fraction())
            .text(text::percent_line(p.fraction(), p.bytes_per_sec))
            .desired_width(ui.available_width().min(DIALOG_WIDTH)),
    );
    ui.label(
        RichText::new(text::file_line(&stage, p.file_done, p.file_total))
            .color(MUTED)
            .small(),
    );
}
