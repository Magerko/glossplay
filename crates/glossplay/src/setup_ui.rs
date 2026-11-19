//! The speech recognition setup on screen: the install dialog, its part of the Settings
//! window, and what their buttons do.

use crate::app::App;
use crate::asr_setup::{self, AsrInstall, ENGINE_AVAILABLE, Engine, Hardware, MODELS, Variant};
use crate::components::{self, Event, InstallError, Progress};
use crate::i18n::{Tr, t, tf};
use crate::jobs::Msg;
use crate::setup_text as text;
use crate::theme::{ACCENT, DANGER, MUTED, SUB_SECONDARY};
use crate::ui::dialog_height;
use eframe::egui::{self, Align, Align2, RichText, vec2};
use gp_core::config::Config;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DIALOG_WIDTH: f32 = 520.0;

enum Outcome {
    Ready { started: bool },
    Failed(String),
    Cancelled,
}

#[derive(Default)]
pub struct AsrSetup {
    pub dialog_open: bool,
    /// Transcribe the current video as soon as the install is done.
    start_after: bool,
    cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<Progress>,
    outcome: Option<Outcome>,
    pub installed: Option<AsrInstall>,
    /// Scroll the Settings window to this section on its next frame.
    pub scroll_to: bool,
}

impl AsrSetup {
    pub fn new() -> AsrSetup {
        AsrSetup {
            installed: asr_setup::installed(),
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
    /// The "Transcribe speech" command: starts recognition, or offers to install it first.
    pub fn request_asr(&mut self) {
        if self.asr_configured().is_ok() {
            return self.start_asr();
        }
        self.open_asr_setup(self.session.is_some());
    }

    pub fn open_asr_setup(&mut self, start_after: bool) {
        let s = &mut self.asr_setup;
        s.dialog_open = true;
        s.start_after |= start_after;
        if !s.running() {
            s.outcome = None;
        }
        s.installed = asr_setup::installed();
        let _ = asr_setup::hardware();
    }

    pub fn install_asr(&mut self, v: Variant) {
        if self.asr_setup.running() || components::is_installing(asr_setup::ID) {
            return;
        }
        // A running whisper-server keeps its files open, and Windows will not replace them.
        self.stop_asr();
        gp_media::tools::stop_named(gp_media::tools::WHISPER);
        if let Err(e) = crate::paths::save_config(&self.cfg) {
            self.toast(tf(Tr::CouldNotSaveSettings, &[&e]));
        }
        let component = asr_setup::component(&v);
        let cancel = Arc::new(AtomicBool::new(false));
        self.asr_setup.cancel = Some(cancel.clone());
        self.asr_setup.progress = Some(Progress::default());
        self.asr_setup.outcome = None;
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
                        id: asr_setup::ID.into(),
                        event: Event::Progress(p.clone()),
                    });
                }
            });
            Msg::Component {
                id: asr_setup::ID.into(),
                event: Event::Done(result),
            }
        });
    }

    fn remove_asr(&mut self) {
        self.stop_asr();
        gp_media::tools::stop_named(gp_media::tools::WHISPER);
        match components::remove(asr_setup::ID) {
            Ok(()) => self.toast(t(Tr::ToastRemoved)),
            Err(e) => self.toast(e),
        }
        self.asr_setup.installed = asr_setup::installed();
        self.asr_setup.outcome = None;
    }

    pub fn on_component_event(&mut self, id: &str, event: Event) {
        if id != asr_setup::ID {
            return;
        }
        let result = match event {
            Event::Progress(p) => {
                self.asr_setup.progress = Some(p);
                return;
            }
            Event::Done(result) => result,
        };
        let s = &mut self.asr_setup;
        s.cancel = None;
        s.progress = None;
        s.installed = asr_setup::installed();
        match result {
            Ok(_) => {
                let start = std::mem::take(&mut s.start_after) && self.session.is_some();
                s.outcome = Some(Outcome::Ready { started: start });
                self.toast(t(Tr::ToastReady));
                if start {
                    self.start_asr();
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

    /// What the buttons of the Settings section asked for; `draft` holds the choices made there.
    pub fn asr_settings_action(&mut self, action: Action, draft: &Config) {
        match action {
            Action::OpenDialog => {
                self.cfg.asr.install_engine = draft.asr.install_engine.clone();
                self.cfg.asr.install_model = draft.asr.install_model.clone();
                self.open_asr_setup(false);
            }
            Action::Remove => self.remove_asr(),
            Action::Cancel => self.cancel_asr_install(),
        }
    }

    fn cancel_asr_install(&mut self) {
        if let Some(c) = &self.asr_setup.cancel {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn asr_setup_dialog(&mut self, ctx: &egui::Context) {
        if !self.asr_setup.dialog_open {
            return;
        }
        let hw = asr_setup::hardware();
        if hw.is_none() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let (mut engine, mut model) = (
            self.cfg.asr.install_engine.clone(),
            self.cfg.asr.install_model.clone(),
        );
        let (mut open, mut install, mut cancel, mut close) = (true, None, false, false);
        let s = &self.asr_setup;
        egui::Window::new(t(Tr::DialogTitle))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(dialog_height(ctx))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(DIALOG_WIDTH);
                        ui.label(t(Tr::DialogIntro));
                        ui.add_space(10.0);
                        let Some(hw) = hw else {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(t(Tr::CheckingHardware));
                            });
                            return;
                        };
                        hardware_line(ui, hw);
                        ui.add_space(4.0);
                        ui.add_enabled_ui(!s.running(), |ui| {
                            choices(ui, "setup-dialog", &mut engine, &mut model, hw);
                        });
                        let v = asr_setup::resolve(&engine, &model, hw.gpu.as_ref());
                        ui.add_space(6.0);
                        plan(ui, &v);
                        warnings(ui, &engine, &v, hw);
                        if let Some(i) = &s.installed {
                            ui.label(RichText::new(i.status()).color(MUTED));
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(t(Tr::LicenceNote)).color(MUTED).small());
                        ui.add_space(10.0);
                        if let Some(p) = &s.progress {
                            progress(ui, p);
                            ui.add_space(6.0);
                            if ui.button(t(Tr::Cancel)).clicked() {
                                cancel = true;
                            }
                            return;
                        }
                        match &s.outcome {
                            Some(Outcome::Ready { started }) => {
                                ui.label(RichText::new(t(Tr::Ready)).color(ACCENT).strong());
                                ui.label(if *started {
                                    t(Tr::ReadyStarted)
                                } else {
                                    t(Tr::ReadyHow)
                                });
                                ui.add_space(6.0);
                                if ui.button(t(Tr::Close)).clicked() {
                                    close = true;
                                }
                                return;
                            }
                            Some(Outcome::Failed(e)) => {
                                ui.label(RichText::new(e).color(DANGER));
                            }
                            Some(Outcome::Cancelled) => {
                                ui.label(RichText::new(t(Tr::Cancelled)).color(MUTED));
                            }
                            None => {}
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let label = match s.outcome {
                                Some(Outcome::Failed(_)) => t(Tr::TryAgain),
                                _ => t(Tr::Install),
                            };
                            if ui.button(RichText::new(label).strong()).clicked() {
                                install = Some(v);
                            }
                            if ui.button(t(Tr::Cancel)).clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if s.running() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.cfg.asr.install_engine = engine;
        self.cfg.asr.install_model = model;
        if cancel {
            self.cancel_asr_install();
        }
        if let Some(v) = install {
            self.install_asr(v);
        }
        if close || !open {
            // Closing only hides the dialog; an install in progress carries on.
            self.asr_setup.dialog_open = false;
            if !self.asr_setup.running() {
                self.asr_setup.start_after = false;
            }
        }
    }
}

/// The Speech recognition part of the Settings window, above the manual paths.
pub fn settings_section(ui: &mut egui::Ui, draft: &mut Config, s: &AsrSetup) -> Option<Action> {
    if s.scroll_to {
        // A little above the cursor, so the section heading stays in view.
        let at = egui::Rect::from_min_size(ui.cursor().min - vec2(0.0, 32.0), vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(Align::TOP));
    }
    let mut action = None;
    let status = match &s.installed {
        Some(i) => i.status(),
        None => t(Tr::NotInstalledStatus).to_owned(),
    };
    ui.label(status);
    if let Some(p) = &s.progress {
        progress(ui, p);
    }
    match asr_setup::hardware() {
        Some(hw) => {
            hardware_line(ui, hw);
            ui.add_enabled_ui(!s.running(), |ui| {
                choices(
                    ui,
                    "settings",
                    &mut draft.asr.install_engine,
                    &mut draft.asr.install_model,
                    hw,
                );
            });
            let v = asr_setup::resolve(
                &draft.asr.install_engine,
                &draft.asr.install_model,
                hw.gpu.as_ref(),
            );
            let size = asr_setup::component(&v).download_size();
            ui.label(
                RichText::new(format!("{}: {}", t(Tr::RowTotal), text::size(size)))
                    .color(MUTED)
                    .small(),
            );
        }
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t(Tr::CheckingHardware));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
    }
    ui.horizontal(|ui| {
        if s.running() {
            if ui.button(t(Tr::Cancel)).clicked() {
                action = Some(Action::Cancel);
            }
            return;
        }
        let label = if s.installed.is_some() {
            t(Tr::Reinstall)
        } else {
            t(Tr::Install)
        };
        if ui.button(label).clicked() {
            action = Some(Action::OpenDialog);
        }
        if s.installed.is_some() && ui.button(t(Tr::Remove)).clicked() {
            action = Some(Action::Remove);
        }
    });
    ui.add_space(6.0);
    action
}

fn engine_name(key: &str, recommended: Engine) -> String {
    match Engine::parse(key) {
        Some(Engine::Nvidia) => t(Tr::EngineNvidia).to_owned(),
        Some(Engine::Cpu) => t(Tr::EngineCpu).to_owned(),
        None => text::engine_auto_is(match recommended {
            Engine::Nvidia => t(Tr::EngineNvidia),
            Engine::Cpu => t(Tr::EngineCpu),
        }),
    }
}

fn model_name(id: &str) -> String {
    match asr_setup::model(id) {
        Some(m) => format!(
            "{} — {}, {}",
            m.id,
            text::size(m.size),
            text::model_hint(m.id)
        ),
        None => id.to_owned(),
    }
}

/// The engine and model pickers.
fn choices(ui: &mut egui::Ui, salt: &str, engine: &mut String, model: &mut String, hw: &Hardware) {
    let gpu = hw.gpu.as_ref();
    let recommended = asr_setup::recommend(gpu).engine;
    egui::Grid::new((salt, "choices"))
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label(t(Tr::RowEngine));
            if ENGINE_AVAILABLE {
                egui::ComboBox::from_id_salt((salt, "engine"))
                    .width(300.0)
                    .selected_text(engine_name(engine, recommended))
                    .show_ui(ui, |ui| {
                        for key in ["auto", "nvidia", "cpu"] {
                            let current = Engine::parse(engine).map_or("auto", Engine::key);
                            if ui
                                .selectable_label(current == key, engine_name(key, recommended))
                                .clicked()
                            {
                                *engine = key.to_owned();
                            }
                        }
                    });
            } else {
                ui.label(t(Tr::EngineOwn));
            }
            ui.end_row();
            ui.label(t(Tr::RowModel));
            let default = asr_setup::resolve(engine, "", gpu).model;
            let auto = format!("{} ({})", t(Tr::ModelAuto), model_name(default.id));
            let selected = if asr_setup::model(model).is_some() {
                model_name(model)
            } else {
                auto.clone()
            };
            egui::ComboBox::from_id_salt((salt, "model"))
                .width(300.0)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(asr_setup::model(model).is_none(), auto)
                        .clicked()
                    {
                        model.clear();
                    }
                    for m in &MODELS {
                        if ui
                            .selectable_label(model == m.id, model_name(m.id))
                            .clicked()
                        {
                            *model = m.id.to_owned();
                        }
                    }
                });
            ui.end_row();
        });
}

fn hardware_line(ui: &mut egui::Ui, hw: &Hardware) {
    let line = match &hw.gpu {
        Some(g) => text::gpu_line(&g.name, g.vram_mb),
        None => t(Tr::NoNvidia).to_owned(),
    };
    ui.label(RichText::new(line).color(MUTED));
}

/// What will be downloaded, how much, and where it goes.
fn plan(ui: &mut egui::Ui, v: &Variant) {
    let c = asr_setup::component(v);
    egui::Grid::new("setup-plan")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for f in &c.files {
                ui.label(&f.label);
                ui.label(text::size(f.size));
                ui.end_row();
            }
            ui.label(RichText::new(t(Tr::RowTotal)).strong());
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
            t(Tr::RowPlace),
            components::dir(&c.id).display()
        ))
        .color(MUTED)
        .small(),
    );
}

fn warnings(ui: &mut egui::Ui, engine_pref: &str, v: &Variant, hw: &Hardware) {
    let mut warn = |t: &str| {
        ui.label(RichText::new(t).color(SUB_SECONDARY));
    };
    if !ENGINE_AVAILABLE {
        warn(t(Tr::NoEngineHere));
        return;
    }
    match v.engine {
        Engine::Cpu => {
            if Engine::parse(engine_pref).is_none() && asr_setup::old_driver(hw.gpu.as_ref()) {
                warn(t(Tr::OldDriver));
            }
            warn(t(Tr::CpuSlow));
        }
        Engine::Nvidia if hw.gpu.is_none() => warn(t(Tr::NvidiaForcedWithoutGpu)),
        Engine::Nvidia => {}
    }
    if asr_setup::vc_runtime_missing() {
        warn(t(Tr::VcRuntimeMissing));
    }
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
    // Checking and unpacking take a while on big files but move nothing on the main bar.
    if matches!(
        p.stage,
        Some(components::Stage::Verifying | components::Stage::Unpacking)
    ) {
        ui.add(
            egui::ProgressBar::new(p.file_fraction())
                .desired_height(4.0)
                .desired_width(ui.available_width().min(DIALOG_WIDTH)),
        );
    }
}
