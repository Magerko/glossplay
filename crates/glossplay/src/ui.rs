use crate::app::{App, Async, SidebarTab};
use crate::i18n::{Tr, t, tf};
use crate::keys;
use crate::session::{Load, Origin, Secondary};
use crate::theme::{
    self, ACCENT, BG, DANGER, LINE, MUTED, PANEL, RAISED, SUB_PRIMARY, SUB_SECONDARY, TEXT,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Id, Key, Layout, Rect, RichText, Sense, Stroke, pos2,
    vec2,
};
use gp_core::{Ms, TokenKind};
use std::sync::Arc;
use std::time::Duration;

const CONTROLS_HEIGHT: f32 = 78.0;
const HIDE_AFTER: Duration = Duration::from_millis(2500);
const VOLUME_WIDTH: f32 = 80.0;
/// Ids of the text fields the self-test types into.
pub const URL_FIELD: &str = "url-field";
pub const TRANSCRIPT_FIELD: &str = "transcript-search";

/// Which items of the control bar are drawn at the current width.
struct ControlsFit {
    volume: bool,
    speed: bool,
    autopause: bool,
    settings: bool,
    sidebar: bool,
    fullscreen: bool,
    full_time: bool,
}

impl ControlsFit {
    fn all() -> Self {
        ControlsFit {
            volume: true,
            speed: true,
            autopause: true,
            settings: true,
            sidebar: true,
            fullscreen: true,
            full_time: true,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        self.handle_input(&ctx);
        self.panel_cues.clear();
        if self.sidebar_open {
            // In a small window the panel takes at most 40%, or the video would shrink to a strip.
            let max = (ctx.content_rect().width() * 0.4).clamp(220.0, 620.0);
            egui::Panel::right("sidebar")
                .default_size(380.0_f32.min(max))
                .size_range(220.0..=max)
                .frame(egui::Frame::NONE.fill(PANEL).inner_margin(12))
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| self.stage(ui));
        self.word_popup(&ctx);
        self.search_window(&ctx);
        self.settings_window(&ctx);
        self.asr_setup_dialog(&ctx);
        self.tts_setup_dialog(&ctx);
        self.toast_ui(&ctx);
        crate::selftest::step(self, &ctx);
        crate::control::step(self, &ctx);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        self.stop_asr();
        self.dub = None;
        self.forget_added_torrent();
        self.jobs.shutdown();
        self.video.destroy(gl);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.quiet {
            // A test window can have the focus while someone types elsewhere: none of their
            // keys, and none of their clipboard, reach it.
            raw.events.retain(|e| !keys::is_keyboard(e));
        }
        crate::selftest::inject(self, ctx, raw);
    }
}

fn icon_button(ui: &mut egui::Ui, icon: &str, hint: &str) -> bool {
    ui.add(
        egui::Button::new(RichText::new(icon).size(17.0))
            .frame(false)
            .min_size(vec2(28.0, 28.0)),
    )
    .on_hover_text(hint)
    .clicked()
}

/// The font's play and pause glyphs differ in size, so I draw this pair myself.
fn play_button(ui: &mut egui::Ui, paused: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
    let color = if resp.hovered() { ACCENT } else { TEXT };
    let c = rect.center();
    if paused {
        let points = vec![
            c + vec2(-6.0, -8.0),
            c + vec2(8.0, 0.0),
            c + vec2(-6.0, 8.0),
        ];
        ui.painter()
            .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        for x in [-6.0, 2.0] {
            let bar = Rect::from_min_size(c + vec2(x, -7.5), vec2(4.0, 15.0));
            ui.painter().rect_filled(bar, 1.0, color);
        }
    }
    resp.on_hover_text(if paused {
        t(Tr::PlayHint)
    } else {
        t(Tr::PauseHint)
    })
    .clicked()
}

fn toggle_chip(ui: &mut egui::Ui, on: bool, label: &str, hint: &str) -> bool {
    let color = if on { ACCENT } else { MUTED };
    let button = egui::Button::new(RichText::new(label).size(13.0).color(color))
        .stroke(Stroke::new(1.0, if on { ACCENT } else { LINE }))
        .fill(Color32::TRANSPARENT);
    ui.add(button).on_hover_text(hint).clicked()
}

impl App {
    fn handle_input(&mut self, ctx: &egui::Context) {
        // A self-test window can take the focus while someone types elsewhere; their keys
        // never get here (see `raw_input_hook`), and their drops and mouse must not either.
        if !self.quiet {
            let dropped: Vec<String> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_string_lossy().into_owned())
                    .filter(|p| !p.is_empty())
                    .collect()
            });
            for path in dropped {
                self.open(&path);
            }
            if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed()) {
                self.last_activity = std::time::Instant::now();
            }
        }
        let gate = self.key_gate(ctx);
        let actions = ctx.input(|i| keys::actions(&i.events, gate));
        for action in actions {
            // A self-test must not open a system dialog or take over the user's screen.
            let skip = self.selftest.is_some()
                && matches!(
                    action,
                    keys::Action::Run(keys::Command::OpenFile | keys::Command::Fullscreen)
                );
            let done = if skip {
                action.name().to_owned()
            } else {
                self.perform(action, ctx)
            };
            if self.selftest.is_some() {
                self.key_log.push(done);
            }
        }
    }

    pub fn key_gate(&self, ctx: &egui::Context) -> keys::Gate {
        keys::Gate {
            typing: ctx.egui_wants_keyboard_input(),
            modal: self.modal_open(),
        }
    }

    /// Runs a shortcut; returns what was done, for the self-test and MCP.
    pub fn perform(&mut self, action: keys::Action, ctx: &egui::Context) -> String {
        match action {
            keys::Action::Run(c) => {
                self.run_command(c, ctx);
                c.name().to_owned()
            }
            keys::Action::Paste(text) => format!("paste_{}", self.open_pasted(&text)),
        }
    }

    pub fn run_command(&mut self, command: keys::Command, ctx: &egui::Context) {
        use keys::Command as C;
        let mpv = self.player.mpv.clone();
        match command {
            C::PlayPause => {
                self.close_popup();
                let paused = mpv.get_flag("pause").unwrap_or(self.snap.paused);
                self.player.set_paused(!paused);
            }
            C::Back => self.player.seek_relative(-5_000),
            C::Forward => self.player.seek_relative(5_000),
            C::VolumeUp => self.set_volume(self.volume + 5.0),
            C::VolumeDown => self.set_volume(self.volume - 5.0),
            C::PreviousLine => self.cue_nav(-1),
            C::RepeatLine => self.cue_nav(0),
            C::NextLine => self.cue_nav(1),
            C::AutoPause => self.toggle_autopause(),
            C::Fullscreen => self.toggle_fullscreen(ctx),
            C::Sidebar => self.sidebar_open = !self.sidebar_open,
            C::Mute => {
                let muted = mpv.get_flag("mute").unwrap_or(self.snap.muted);
                mpv.set_flag("mute", !muted || self.quiet);
            }
            C::ToggleOriginal => {
                self.cfg.subtitles.show_primary = !self.cfg.subtitles.show_primary;
            }
            C::ToggleTranslation => {
                self.cfg.subtitles.show_secondary = !self.cfg.subtitles.show_secondary;
            }
            C::Slower | C::Faster => {
                let speed = mpv.get_f64("speed").unwrap_or(self.snap.speed);
                let speed = if command == C::Slower {
                    (speed - 0.25).max(0.25)
                } else {
                    (speed + 0.25).min(4.0)
                };
                mpv.set_f64("speed", speed);
            }
            C::OpenFile => self.open_dialog(),
            C::Search => self.open_search(None),
            // The top window first: the setup dialogs open over Settings, and all of them
            // over the word card.
            C::Escape => {
                if self.asr_setup.dialog_open {
                    self.close_asr_setup();
                } else if self.tts_setup.dialog_open {
                    self.close_tts_setup();
                } else if self.settings_draft.is_some() {
                    self.settings_draft = None;
                } else if self.search_ui.open {
                    self.close_search();
                } else if self.popup.is_some() {
                    self.close_popup();
                } else if self.fullscreen {
                    self.toggle_fullscreen(ctx);
                }
            }
        }
    }

    /// Ctrl+V outside a text field: opens a copied link, magnet link or file path the way
    /// dropping a file or typing a link does. Returns which it was.
    pub fn open_pasted(&mut self, text: &str) -> &'static str {
        match keys::classify(text, |p| std::path::Path::new(p).is_file()) {
            keys::Pasted::Link(url) => {
                self.open(&url);
                "link"
            }
            keys::Pasted::File(path) => {
                self.open(&path);
                "file"
            }
            keys::Pasted::Magnet(magnet) => {
                if self.torrserver_ready() {
                    self.open_magnet(magnet);
                    "magnet"
                } else {
                    self.toast(t(Tr::PasteNeedsTorrServer));
                    "magnet_without_torrserver"
                }
            }
            keys::Pasted::Nothing => {
                self.toast(t(Tr::PasteNothing));
                "nothing"
            }
        }
    }

    fn toggle_autopause(&mut self) {
        self.trainer.autopause = !self.trainer.autopause;
        self.cfg.training.autopause = self.trainer.autopause;
        self.toast(if self.trainer.autopause {
            t(Tr::AutoPauseOn)
        } else {
            t(Tr::AutoPauseOff)
        });
    }

    /// A dialog or Settings is open: the player under it takes no clicks and no shortcuts.
    pub fn modal_open(&self) -> bool {
        self.settings_draft.is_some()
            || self.asr_setup.dialog_open
            || self.tts_setup.dialog_open
            || self.search_ui.open
    }

    pub fn controls_visible(&self) -> bool {
        if let Some(fixed) = crate::control::controls_override() {
            return fixed;
        }
        self.snap.paused
            || self.popup.is_some()
            || self.last_activity.elapsed() < HIDE_AFTER
            || self.selftest.as_ref().is_some_and(|s| s.plan.show_controls)
    }

    fn stage(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        if self.session.is_none() {
            return self.empty_state(ui, rect);
        }
        let sense = if self.modal_open() {
            Sense::hover()
        } else {
            Sense::click()
        };
        let response = ui.interact(rect, Id::unique("video"), sense);
        if response.double_clicked() {
            self.toggle_fullscreen(ui.ctx());
        } else if response.clicked() {
            if self.popup.is_some() {
                self.close_popup();
            } else {
                self.player.set_paused(!self.snap.paused);
            }
        }
        self.video.paint(ui, rect);

        let visible = self.controls_visible();
        if !visible {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        } else if !self.snap.paused {
            ui.ctx().request_repaint_after(HIDE_AFTER);
        }
        // A tall window (a vertical recording, a phone-shaped layout) must not blow the text up.
        let scale = (rect.height() / 720.0)
            .min(rect.width() / 1280.0)
            .clamp(0.6, 3.0);
        let bottom = rect.bottom()
            - if visible {
                CONTROLS_HEIGHT + 14.0
            } else {
                34.0 * scale
            };
        self.subtitles(ui, rect, bottom, scale);
        if visible {
            self.header(ui, rect);
            self.controls(ui, rect);
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(rect.center(), vec2(460.0, 320.0)))
                .layout(Layout::top_down(Align::Center)),
        );
        let ui = &mut child;
        ui.label(RichText::new("Glossplay").size(34.0).strong());
        ui.label(RichText::new(t(Tr::Tagline)).color(MUTED));
        ui.add_space(18.0);
        if ui
            .add(
                egui::Button::new(RichText::new(t(Tr::OpenVideo)).size(16.0))
                    .min_size(vec2(200.0, 36.0)),
            )
            .clicked()
        {
            self.open_dialog();
        }
        ui.label(RichText::new(t(Tr::DropHint)).color(MUTED).small());
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let field = egui::TextEdit::singleline(&mut self.url_input)
                .id(Id::unique(URL_FIELD))
                .hint_text(t(Tr::PasteLink))
                .desired_width(340.0);
            let entered = ui.add(field).lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.button(t(Tr::PlayButton)).clicked() || entered)
                && !self.url_input.trim().is_empty()
            {
                let url = self.url_input.trim().to_owned();
                self.open(&url);
            }
        });
        if self.search_available() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let field = egui::TextEdit::singleline(&mut self.search_ui.query)
                    .hint_text(t(Tr::SearchField))
                    .desired_width(340.0);
                let entered = ui.add(field).lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if ui.button(t(Tr::SearchButton)).clicked() || entered {
                    let query = self.search_ui.query.clone();
                    self.open_search(Some(query));
                }
            });
        }
        ui.add_space(18.0);
        ui.label(RichText::new(t(Tr::ShortcutsHelp)).color(MUTED).small());
        if self.search_available() {
            ui.label(RichText::new(t(Tr::ShortcutsSearch)).color(MUTED).small());
        }
        ui.add_space(8.0);
        if ui.link(t(Tr::SettingsLink)).clicked() {
            self.settings_draft = Some(self.cfg.clone());
        }
        if self.asr_setup.installed.is_none() {
            ui.add_space(4.0);
            if ui.link(t(Tr::SetUpLink)).clicked() {
                self.open_asr_setup(false);
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(s) = &self.session else { return };
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
        ui.painter()
            .rect_filled(strip, 0.0, Color32::from_black_alpha(120));
        let mut text = s.title.clone();
        if !self.hwdec_current.is_empty() && self.hwdec_current != "no" {
            text.push_str(&format!("   ·   {}", self.hwdec_current));
        }
        if !s.asr_status.is_empty() {
            text.push_str(&format!("   ·   {}", s.asr_status));
        }
        let pending = s
            .tracks
            .iter()
            .filter(|t| t.load == Load::Loading && t.origin != Origin::Asr)
            .count();
        if pending > 0 {
            text.push_str(&format!("   ·   {}", t(Tr::ReadingSubtitles)));
        }
        if let Some(d) = &self.dub {
            text.push_str(&format!("   ·   {}", d.status()));
        }
        ui.painter().text(
            strip.left_center() + vec2(16.0, 0.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(14.0),
            TEXT,
        );
    }

    fn subtitles(&mut self, ui: &mut egui::Ui, rect: Rect, bottom: f32, scale: f32) {
        let Some(s) = &self.session else { return };
        let pos = self.snap.pos;
        let primary = if self.cfg.subtitles.show_primary {
            s.primary_lines(pos)
        } else {
            Vec::new()
        };
        let secondary = if self.cfg.subtitles.show_secondary {
            s.secondary_lines(pos)
        } else {
            Vec::new()
        };
        let track_index = s.primary.unwrap_or(0);
        let lang = s.primary_track().and_then(|t| t.track.lang.clone());
        let size = self.cfg.subtitles.font_size * scale;
        let max_width = rect.width() * 0.86;
        let mut y = bottom;

        let secondary_lang = match s.secondary {
            Secondary::Track(i) => s.tracks.get(i).and_then(|t| t.track.lang.clone()),
            _ => Some(self.cfg.general.native_lang.clone()),
        };
        let small_size = size * self.cfg.subtitles.secondary_scale;
        for text in secondary.iter().rev() {
            let small = FontId::new(
                small_size,
                theme::subs_family(secondary_lang.as_deref(), text),
            );
            let galley =
                ui.painter()
                    .layout(text.replace('\n', " "), small, SUB_SECONDARY, max_width);
            let at = pos2(rect.center().x - galley.size().x / 2.0, y - galley.size().y);
            let backdrop = Rect::from_min_size(at, galley.size()).expand2(vec2(10.0, 3.0));
            ui.painter()
                .rect_filled(backdrop, 6.0, Color32::from_black_alpha(140));
            outlined(ui.painter(), at, galley.clone(), SUB_SECONDARY, scale);
            y -= galley.size().y + 8.0;
        }

        let open_token = self.popup.as_ref().map(|p| p.token);
        let words_live = !self.modal_open();
        let mut clicked: Option<(String, usize, usize, egui::Pos2)> = None;
        for line in primary.iter().rev() {
            let tokens = self.tokens_for(track_index, line.cue, &line.text, lang.as_deref());
            let font = FontId::new(size, theme::subs_family(lang.as_deref(), &line.text));
            // Lay the words out in rows by hand: every word needs its own hit box.
            let mut rows: Vec<Vec<(usize, Arc<egui::Galley>)>> = vec![Vec::new()];
            let mut row_width = 0.0;
            for (i, token) in tokens.iter().enumerate() {
                let piece = &line.text[token.range.clone()];
                if piece.contains('\n') {
                    rows.push(Vec::new());
                    row_width = 0.0;
                    continue;
                }
                let galley =
                    ui.painter()
                        .layout_no_wrap(piece.to_owned(), font.clone(), SUB_PRIMARY);
                let width = galley.size().x;
                if row_width + width > max_width && row_width > 0.0 {
                    rows.push(Vec::new());
                    row_width = 0.0;
                }
                if row_width == 0.0 && piece.trim().is_empty() {
                    continue;
                }
                row_width += width;
                rows.last_mut().unwrap().push((i, galley));
            }
            rows.retain(|r| !r.is_empty());
            let row_height = size * 1.3;
            let block_height = rows.len() as f32 * row_height;
            let widest = rows
                .iter()
                .map(|r| r.iter().map(|(_, g)| g.size().x).sum::<f32>())
                .fold(0.0, f32::max);
            let block = Rect::from_min_size(
                pos2(rect.center().x - widest / 2.0, y - block_height),
                vec2(widest, block_height),
            );
            ui.painter().rect_filled(
                block.expand2(vec2(12.0, 4.0)),
                8.0,
                Color32::from_black_alpha(140),
            );
            for (r, row) in rows.iter().enumerate() {
                let width: f32 = row.iter().map(|(_, g)| g.size().x).sum();
                let mut x = rect.center().x - width / 2.0;
                let top = block.top() + r as f32 * row_height;
                for (i, galley) in row {
                    let token = &tokens[*i];
                    let word_rect =
                        Rect::from_min_size(pos2(x, top), vec2(galley.size().x, row_height));
                    let at = pos2(x, top + (row_height - galley.size().y) / 2.0);
                    x += galley.size().x;
                    if token.kind != TokenKind::Word {
                        outlined(ui.painter(), at, galley.clone(), SUB_PRIMARY, scale);
                        continue;
                    }
                    let word = &line.text[token.range.clone()];
                    crate::control::word_drawn(track_index, line.cue, *i, word_rect);
                    let resp = words_live.then(|| {
                        ui.interact(
                            word_rect,
                            Id::unique(("word", line.cue, *i)),
                            Sense::click(),
                        )
                    });
                    let hovered = resp.as_ref().is_some_and(|r| r.hovered());
                    let selected = open_token == Some((line.cue, *i));
                    if hovered || selected {
                        ui.painter().rect_filled(
                            word_rect.expand2(vec2(2.0, 0.0)),
                            5.0,
                            ACCENT.gamma_multiply(0.45),
                        );
                    }
                    if hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    outlined(ui.painter(), at, galley.clone(), SUB_PRIMARY, scale);
                    if self.saved_words.contains(&word.to_lowercase()) {
                        let under = word_rect.bottom() - row_height * 0.1;
                        ui.painter().line_segment(
                            [
                                pos2(word_rect.left(), under),
                                pos2(word_rect.right(), under),
                            ],
                            Stroke::new(2.0 * scale, ACCENT),
                        );
                    }
                    if resp.is_some_and(|r| r.clicked()) {
                        clicked = Some((word.to_owned(), line.cue, *i, word_rect.center_top()));
                    }
                }
            }
            y -= block_height + 10.0;
        }
        if let Some((word, cue, token, anchor)) = clicked {
            self.open_word(word, cue, token, anchor);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - CONTROLS_HEIGHT), rect.max);
        ui.painter()
            .rect_filled(bar, 0.0, Color32::from_black_alpha(190));
        // Swallow clicks on the bar so they do not toggle pause through to the video.
        ui.interact(bar, Id::unique("controls-bg"), Sense::click());
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar.shrink2(vec2(16.0, 8.0)))
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut child;
        self.seek_bar(ui);
        ui.add_space(2.0);
        let fit = self.fit_controls(ui, ui.available_width());
        let clock = if fit.full_time {
            format!(
                "{} / {}",
                theme::clock(self.snap.pos),
                theme::clock(self.snap.duration)
            )
        } else {
            theme::clock(self.snap.pos)
        };
        ui.horizontal(|ui| {
            if icon_button(ui, "⏮", t(Tr::PrevLineHint)) {
                self.cue_nav(-1);
            }
            if play_button(ui, self.snap.paused) {
                self.close_popup();
                self.player.set_paused(!self.snap.paused);
            }
            if icon_button(ui, "⏭", t(Tr::NextLineHint)) {
                self.cue_nav(1);
            }
            if icon_button(ui, "🔁", t(Tr::RepeatLineHint)) {
                self.cue_nav(0);
            }
            ui.label(RichText::new(clock).color(MUTED).monospace());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if fit.fullscreen && icon_button(ui, "⛶", t(Tr::FullscreenHint)) {
                    self.toggle_fullscreen(ui.ctx());
                }
                if fit.settings && icon_button(ui, "⚙", t(Tr::SettingsHint)) {
                    self.settings_draft = Some(self.cfg.clone());
                }
                if fit.settings
                    && self.search_available()
                    && icon_button(ui, "🔍", t(Tr::SearchVideosHint))
                {
                    self.open_search(None);
                }
                if fit.sidebar && icon_button(ui, "☰", t(Tr::SidebarHint)) {
                    self.sidebar_open = !self.sidebar_open;
                }
                self.subtitle_menu(ui, &fit);
                if fit.autopause
                    && toggle_chip(
                        ui,
                        self.trainer.autopause,
                        t(Tr::AutoPause),
                        t(Tr::AutoPauseHint),
                    )
                {
                    self.toggle_autopause();
                }
                if fit.speed {
                    ui.menu_button(
                        RichText::new(format!("{:.2}×", self.snap.speed)).size(13.0),
                        |ui| {
                            for speed in [0.5, 0.75, 1.0, 1.25, 1.5, 2.0] {
                                if ui
                                    .selectable_label(self.snap.speed == speed, format!("{speed}×"))
                                    .clicked()
                                {
                                    self.player.mpv.set_f64("speed", speed);
                                    ui.close();
                                }
                            }
                        },
                    );
                }
                if fit.volume {
                    let mut volume = self.volume as f32;
                    ui.spacing_mut().slider_width = VOLUME_WIDTH;
                    if ui
                        .add(egui::Slider::new(&mut volume, 0.0..=150.0).show_value(false))
                        .changed()
                    {
                        self.set_volume(volume as f64);
                    }
                }
                if icon_button(
                    ui,
                    if self.snap.muted { "🔇" } else { "🔊" },
                    t(Tr::MuteHint),
                ) {
                    self.player.mpv.set_flag("mute", !self.snap.muted);
                }
            });
        });
    }

    /// Decides which control-bar items fit into `avail` points. The least important ones go
    /// first, so nothing is ever drawn on top of something else.
    fn fit_controls(&self, ui: &egui::Ui, avail: f32) -> ControlsFit {
        let gap = ui.spacing().item_spacing.x;
        let text_w = |text: String, font: FontId| {
            ui.painter()
                .layout_no_wrap(text, font, Color32::WHITE)
                .size()
                .x
        };
        let chip_w = |text: &str| text_w(text.to_owned(), FontId::proportional(13.0)) + 16.0 + 6.0;
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let full_clock = format!(
            "{} / {}",
            theme::clock(self.snap.pos),
            theme::clock(self.snap.duration)
        );
        // The clock grows with the longest value it can take, so the layout does not jump.
        let widest = |s: &str| text_w(s.replace(|c: char| c.is_ascii_digit(), "0"), mono.clone());
        let full_clock_w = widest(&full_clock);
        let short_clock_w = widest(&theme::clock(self.snap.duration));
        let icon = 28.0;
        let left = 28.0 + 32.0 + 28.0 + 28.0 + 3.0 * gap;
        let speed_w = chip_w("0.00×");
        let autopause_w = chip_w(t(Tr::AutoPause));
        let subtitles_w = chip_w(t(Tr::SubtitlesMenu));
        let search = self.search_available();
        let needed = |f: &ControlsFit| {
            let mut right = vec![icon, subtitles_w];
            for (on, w) in [
                (f.volume, VOLUME_WIDTH + 2.0),
                (f.speed, speed_w),
                (f.autopause, autopause_w),
                (f.settings, icon),
                (f.settings && search, icon),
                (f.sidebar, icon),
                (f.fullscreen, icon),
            ] {
                if on {
                    right.push(w);
                }
            }
            let clock = if f.full_time {
                full_clock_w
            } else {
                short_clock_w
            };
            let right_total = right.iter().sum::<f32>() + gap * (right.len() as f32 - 1.0);
            left + gap + clock + gap * 2.0 + right_total
        };
        let mut fit = ControlsFit::all();
        let drops: [fn(&mut ControlsFit); 7] = [
            |f| f.volume = false,
            |f| f.speed = false,
            |f| f.autopause = false,
            |f| f.full_time = false,
            |f| f.sidebar = false,
            |f| f.settings = false,
            |f| f.fullscreen = false,
        ];
        for drop in drops {
            if needed(&fit) <= avail {
                break;
            }
            drop(&mut fit);
        }
        fit
    }

    fn seek_bar(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::click_and_drag());
        let duration = self.snap.duration.max(1) as f32;
        let track = Rect::from_center_size(
            rect.center(),
            vec2(rect.width(), if resp.hovered() { 6.0 } else { 4.0 }),
        );
        ui.painter()
            .rect_filled(track, 3.0, Color32::from_white_alpha(50));
        let x_of = |ms: Ms| track.left() + track.width() * (ms as f32 / duration).clamp(0.0, 1.0);
        let played = Rect::from_min_max(track.min, pos2(x_of(self.snap.pos), track.bottom()));
        ui.painter().rect_filled(played, 3.0, ACCENT);
        if resp.hovered() || resp.dragged() {
            ui.painter()
                .circle_filled(pos2(x_of(self.snap.pos), track.center().y), 7.0, ACCENT);
        }
        let Some(pointer) = resp.interact_pointer_pos().or(resp.hover_pos()) else {
            return;
        };
        let target =
            (((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * duration) as Ms;
        if resp.hovered() {
            resp.clone().on_hover_text_at_pointer(theme::clock(target));
        }
        if resp.dragged() {
            // Keyframe seeks keep scrubbing fluid; the exact seek happens on release.
            let secs = format!("{:.3}", target as f64 / 1000.0);
            self.player
                .mpv
                .command_async(&["seek", &secs, "absolute+keyframes"]);
        }
        if resp.clicked() || resp.drag_stopped() {
            self.player.seek(target);
        }
    }

    fn subtitle_menu(&mut self, ui: &mut egui::Ui, fit: &ControlsFit) {
        let provider = match &self.translator {
            Ok(t) => t.id(),
            Err(_) => t(Tr::ProviderNotSetUp).to_owned(),
        };
        let asr_running = self.session.as_ref().is_some_and(|s| s.asr_running);
        let mut pick_primary = None;
        let mut pick_secondary = None;
        let mut toggle_asr = false;
        let mut add_file = false;
        let mut save_srt = None;
        let mut pick_audio = None;
        let mut toggle_autopause = false;
        let mut open_settings = false;
        let mut open_search = false;
        let dub_on = self.dub.is_some();
        let (mut toggle_dub, mut volumes_changed, mut volumes_done) = (false, false, false);
        let mut original = self.cfg.tts.original_volume as f32;
        let mut voice = self.cfg.tts.voice_volume as f32;
        let menu = ui.menu_button(RichText::new(t(Tr::SubtitlesMenu)).size(13.0), |ui| {
            let Some(s) = &self.session else { return };
            ui.set_min_width(260.0);
            ui.label(RichText::new(t(Tr::Learning)).color(MUTED).small());
            for (i, sub) in s.tracks.iter().enumerate() {
                let state = match &sub.load {
                    Load::Ready if sub.live => format!("  ({})", t(Tr::StateAsYouWatch)),
                    Load::NotLoaded | Load::Ready => String::new(),
                    Load::Loading => format!("  ({})", t(Tr::StateReading)),
                    Load::Failed(_) => format!("  ({})", t(Tr::StateFailed)),
                    Load::Bitmap => format!("  ({})", t(Tr::StateImage)),
                };
                let resp =
                    ui.selectable_label(s.primary == Some(i), format!("{}{state}", sub.label));
                if let Load::Failed(e) = &sub.load {
                    resp.clone().on_hover_text(e);
                }
                if resp.clicked() {
                    pick_primary = Some(Some(i));
                }
            }
            if ui
                .selectable_label(s.primary.is_none(), t(Tr::NoneChoice))
                .clicked()
            {
                pick_primary = Some(None);
            }
            ui.separator();
            ui.label(RichText::new(t(Tr::TranslationLabel)).color(MUTED).small());
            if ui
                .selectable_label(
                    s.secondary == Secondary::Translate,
                    tf(Tr::TranslateAuto, &[&provider]),
                )
                .clicked()
            {
                pick_secondary = Some(Secondary::Translate);
            }
            for (i, t) in s
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, t)| t.is_text() && s.primary != Some(*i))
            {
                if ui
                    .selectable_label(s.secondary == Secondary::Track(i), &t.label)
                    .clicked()
                {
                    pick_secondary = Some(Secondary::Track(i));
                }
            }
            if ui
                .selectable_label(s.secondary == Secondary::Off, t(Tr::OffChoice))
                .clicked()
            {
                pick_secondary = Some(Secondary::Off);
            }
            ui.separator();
            let label = if asr_running {
                t(Tr::StopTranscribing)
            } else {
                t(Tr::TranscribeSpeech)
            };
            if ui.button(label).clicked() {
                toggle_asr = true;
            }
            if ui
                .selectable_label(dub_on, t(Tr::MenuVoiceOver))
                .on_hover_text(t(Tr::MenuVoiceOverHint))
                .clicked()
            {
                toggle_dub = true;
            }
            if dub_on {
                egui::Grid::new("dub-volumes")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        for (label, value) in [
                            (t(Tr::SliderOriginal), &mut original),
                            (t(Tr::SliderVoice), &mut voice),
                        ] {
                            ui.label(RichText::new(label).color(MUTED));
                            let r =
                                ui.add(egui::Slider::new(value, 0.0..=150.0).integer().suffix("%"));
                            volumes_changed |= r.changed();
                            volumes_done |= r.drag_stopped() || (r.changed() && !r.dragged());
                            ui.end_row();
                        }
                    });
            }
            if ui.button(t(Tr::AddSubFile)).clicked() {
                add_file = true;
            }
            ui.separator();
            let has_primary = s.primary_track().is_some();
            let shown = s.secondary != Secondary::Off;
            if ui
                .add_enabled(has_primary, egui::Button::new(t(Tr::SaveTranscriptSrt)))
                .clicked()
            {
                save_srt = Some(false);
            }
            if shown
                && ui
                    .add_enabled(has_primary, egui::Button::new(t(Tr::SaveTranslationSrt)))
                    .clicked()
            {
                save_srt = Some(true);
            }
            if self.audio_tracks.len() > 1 {
                ui.separator();
                ui.label(RichText::new(t(Tr::AudioLabel)).color(MUTED).small());
                for a in &self.audio_tracks {
                    if ui.selectable_label(a.selected, a.label()).clicked() {
                        pick_audio = Some(a.id);
                    }
                }
            }
            // Items the narrow control bar had no room for live here instead.
            if !fit.autopause || !fit.settings {
                ui.separator();
            }
            if !fit.autopause
                && ui
                    .selectable_label(self.trainer.autopause, t(Tr::AutoPauseMenu))
                    .clicked()
            {
                toggle_autopause = true;
            }
            if !fit.settings && ui.button(t(Tr::SettingsMenu)).clicked() {
                open_settings = true;
            }
            if !fit.settings && self.search_available() && ui.button(t(Tr::SearchMenu)).clicked() {
                open_search = true;
            }
        });
        if self.selftest.as_ref().is_some_and(|s| s.plan.subs_menu) {
            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&menu.response));
        }
        if toggle_dub {
            self.set_dub(!dub_on);
        }
        if volumes_changed {
            self.cfg.tts.original_volume = f64::from(original);
            self.cfg.tts.voice_volume = f64::from(voice);
            self.apply_volume();
        }
        if volumes_done
            && !self.quiet
            && let Err(e) = crate::paths::save_config(&self.cfg)
        {
            self.toast(tf(Tr::CouldNotSaveSettings, &[&e]));
        }
        if toggle_autopause {
            self.toggle_autopause();
        }
        if open_settings {
            self.settings_draft = Some(self.cfg.clone());
        }
        if open_search {
            self.open_search(None);
        }
        if let Some(choice) = pick_primary {
            self.select_primary(choice);
        }
        if let Some(choice) = pick_secondary {
            self.set_secondary(choice);
        }
        if toggle_asr {
            if asr_running {
                self.stop_asr()
            } else {
                self.request_asr()
            }
        }
        if add_file {
            let picked = rfd::FileDialog::new()
                .add_filter(t(Tr::SubtitlesMenu), &["srt", "vtt", "ass", "ssa"])
                .pick_file();
            if let Some(path) = picked {
                self.open(&path.to_string_lossy());
            }
        }
        if let Some(translation) = save_srt {
            self.save_srt(translation);
        }
        if let Some(id) = pick_audio {
            self.player.mpv.set("aid", &id.to_string());
            for a in &mut self.audio_tracks {
                a.selected = a.id == id;
            }
        }
    }

    fn save_srt(&mut self, translation: bool) {
        let Some(s) = &self.session else { return };
        let Some(t) = s.primary_track() else { return };
        let (cues, lang) = if translation {
            (s.translation_cues(), self.cfg.general.native_lang.clone())
        } else {
            (
                t.track.cues.clone(),
                t.track.lang.clone().unwrap_or_default(),
            )
        };
        let title: String = s
            .title
            .chars()
            .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
            .collect();
        let name = if lang.is_empty() {
            format!("{title}.srt")
        } else {
            format!("{title}.{lang}.srt")
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("SubRip", &["srt"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, gp_core::write_srt(&cues)) {
            Ok(()) => self.toast(tf(Tr::SavedLines, &[&cues.len(), &path.display()])),
            Err(e) => self.toast(tf(Tr::CouldNotSaveFile, &[&path.display(), &e])),
        }
    }

    pub fn select_primary(&mut self, choice: Option<usize>) {
        if let Some(i) = choice {
            self.ensure_loaded(i);
        }
        let Some(s) = &mut self.session else { return };
        s.primary = choice;
        self.scrolled_to = None;
        // mpv draws image subtitles (and tracks I could not read) itself; for text tracks its own renderer stays off.
        let bitmap_id = choice.and_then(|i| match (&s.tracks[i].origin, &s.tracks[i].load) {
            (Origin::Embedded { mpv_id, .. }, Load::Bitmap | Load::Failed(_)) => Some(*mpv_id),
            _ => None,
        });
        self.mpv_draws(bitmap_id);
        self.set_live_slots();
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.sidebar_tab,
                SidebarTab::Transcript,
                t(Tr::TabTranscript),
            );
            let words = tf(Tr::TabWords, &[&self.vocab.len()]);
            ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Words, words);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, "✕", t(Tr::CloseHint)) {
                    self.sidebar_open = false;
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(Id::unique(TRANSCRIPT_FIELD))
                .hint_text(t(Tr::SearchHint))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        match self.sidebar_tab {
            SidebarTab::Transcript => self.transcript(ui),
            SidebarTab::Words => self.word_list(ui),
        }
    }

    fn transcript(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else {
            ui.label(RichText::new(t(Tr::OpenToSeeTranscript)).color(MUTED));
            return;
        };
        let Some(t) = s.primary_track() else {
            ui.label(RichText::new(t(Tr::NoTextSubtitles)).color(MUTED));
            return;
        };
        let needle = self.search.to_lowercase();
        let rows: Vec<usize> = (0..t.track.cues.len())
            .filter(|&i| needle.is_empty() || t.track.cues[i].text.to_lowercase().contains(&needle))
            .collect();
        let current = t.track.index_at_or_before(self.snap.pos);
        let row_height = 46.0;
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        // Follow playback, but not while the pointer is over the list: the user is reading it.
        let hovering = ui.rect_contains_pointer(ui.max_rect());
        if current != self.scrolled_to && !hovering && needle.is_empty() {
            if let Some(row) = current.and_then(|c| rows.iter().position(|&i| i == c)) {
                let offset = row as f32 * (row_height + ui.spacing().item_spacing.y)
                    - ui.available_height() / 2.0;
                area = area.vertical_scroll_offset(offset.max(0.0));
            }
            self.scrolled_to = current;
        }
        let native = self.cfg.general.native_lang.clone();
        let mut seek_to = None;
        let mut shown = 0..0;
        area.show_rows(ui, row_height, rows.len(), |ui, range| {
            shown = range.clone();
            for &i in &rows[range] {
                let cue = &t.track.cues[i];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
                if Some(i) == current {
                    ui.painter().rect_filled(rect, 6.0, RAISED);
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                        2.0,
                        ACCENT,
                    );
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, 6.0, RAISED.gamma_multiply(0.6));
                }
                let text_left = rect.left() + 58.0;
                let width = rect.right() - text_left - 6.0;
                ui.painter().text(
                    rect.left_top() + vec2(10.0, 7.0),
                    Align2::LEFT_TOP,
                    theme::clock(cue.start),
                    FontId::monospace(11.0),
                    MUTED,
                );
                let one_line = |text: &str, font: FontId, color: Color32| {
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        text.replace('\n', " "),
                        font,
                        color,
                    );
                    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
                    ui.painter().layout_job(job)
                };
                ui.painter().galley(
                    pos2(text_left, rect.top() + 5.0),
                    one_line(
                        &cue.text,
                        FontId::new(14.5, theme::subs_family(t.track.lang.as_deref(), &cue.text)),
                        TEXT,
                    ),
                    TEXT,
                );
                if let Some(tr) = s.translation_of(i) {
                    ui.painter().galley(
                        pos2(text_left, rect.top() + 25.0),
                        one_line(
                            &tr,
                            FontId::new(12.5, theme::subs_family(Some(&native), &tr)),
                            MUTED,
                        ),
                        MUTED,
                    );
                }
                if resp.clicked() {
                    seek_to = Some(cue.start);
                }
            }
        });
        // A few rows past the edges, so a short scroll finds them translated already.
        const MARGIN: usize = 5;
        let around = shown.start.saturating_sub(MARGIN)..(shown.end + MARGIN).min(rows.len());
        self.panel_cues = rows[around].to_vec();
        if let Some(ms) = seek_to {
            self.player.seek(ms);
        }
    }

    fn word_list(&mut self, ui: &mut egui::Ui) {
        if self.vocab.is_empty() {
            ui.label(RichText::new(t(Tr::WordsEmpty)).color(MUTED));
            return;
        }
        let needle = self.search.to_lowercase();
        let current_media = self.session.as_ref().map(|s| s.media.clone());
        let (mut jump, mut anki, mut delete) = (None, None, None);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for entry in &self.vocab {
                    let w = &entry.word;
                    if !needle.is_empty()
                        && !w.word.to_lowercase().contains(&needle)
                        && !w.translation.to_lowercase().contains(&needle)
                    {
                        continue;
                    }
                    egui::Frame::NONE
                        .fill(RAISED)
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&w.word)
                                        .size(17.0)
                                        .strong()
                                        .family(theme::subs_family(None, &w.word)),
                                );
                                ui.label(RichText::new(&w.translation).color(ACCENT));
                            });
                            ui.label(
                                RichText::new(&w.sentence)
                                    .color(MUTED)
                                    .small()
                                    .family(theme::subs_family(None, &w.sentence)),
                            );
                            ui.horizontal(|ui| {
                                if current_media.as_deref() == Some(w.media_path.as_str())
                                    && ui
                                        .small_button(t(Tr::ShowWord))
                                        .on_hover_text(t(Tr::JumpHint))
                                        .clicked()
                                {
                                    jump = Some(w.cue_start_ms);
                                }
                                let label = if entry.anki_note_id.is_some() {
                                    t(Tr::InAnki)
                                } else {
                                    t(Tr::SendToAnki)
                                };
                                if ui
                                    .add_enabled(
                                        entry.anki_note_id.is_none(),
                                        egui::Button::new(label).small(),
                                    )
                                    .clicked()
                                {
                                    anki = Some(entry.id);
                                }
                                if ui
                                    .small_button(RichText::new(t(Tr::DeleteWord)).color(DANGER))
                                    .clicked()
                                {
                                    delete = Some(entry.id);
                                }
                                ui.label(RichText::new(&w.media_title).color(MUTED).small());
                            });
                        });
                }
            });
        if let Some(ms) = jump {
            self.player.seek(ms);
        }
        if let Some(id) = anki {
            self.export_to_anki(id);
        }
        if let Some(id) = delete {
            self.delete_word(id);
        }
    }

    fn word_popup(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.popup else { return };
        let in_anki = p.saved_id.is_some_and(|id| {
            self.vocab
                .iter()
                .any(|v| v.id == id && v.anki_note_id.is_some())
        });
        let lang = self
            .session
            .as_ref()
            .and_then(|s| s.primary_track())
            .and_then(|t| t.track.lang.clone());
        let (mut save, mut anki, mut close) = (false, false, false);
        // Above the subtitles and the controls, below every window.
        egui::Area::new(Id::unique("word-popup"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(p.anchor - vec2(0.0, 14.0))
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(PANEL)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&p.word)
                                    .size(22.0)
                                    .strong()
                                    .family(theme::subs_family(lang.as_deref(), &p.word)),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, "✕", t(Tr::CloseEscHint)) {
                                    close = true;
                                }
                            });
                        });
                        match &p.translation {
                            Async::Ready(text) => {
                                drop(ui.label(RichText::new(text).size(18.0).color(ACCENT)))
                            }
                            Async::Pending => drop(ui.spinner()),
                            Async::Failed(e) => {
                                drop(ui.label(RichText::new(e).color(DANGER).small()))
                            }
                            Async::Idle => {}
                        }
                        match &p.explanation {
                            Async::Ready(text) => {
                                ui.add_space(4.0);
                                ui.label(text);
                            }
                            Async::Pending => drop(
                                ui.label(RichText::new(t(Tr::Explaining)).color(MUTED).small()),
                            ),
                            Async::Failed(e) => {
                                drop(ui.label(RichText::new(e).color(DANGER).small()))
                            }
                            Async::Idle => {}
                        }
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&p.sentence)
                                .color(MUTED)
                                .size(13.0)
                                .family(theme::subs_family(lang.as_deref(), &p.sentence)),
                        );
                        if !p.sentence_translation.is_empty() {
                            ui.label(
                                RichText::new(&p.sentence_translation)
                                    .color(MUTED)
                                    .size(13.0)
                                    .italics(),
                            );
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let label = if p.saved_id.is_some() {
                                t(Tr::WordSaved)
                            } else {
                                t(Tr::SaveWord)
                            };
                            if ui.button(label).clicked() {
                                save = true;
                            }
                            let label = if in_anki {
                                t(Tr::InAnki)
                            } else {
                                t(Tr::SendToAnki)
                            };
                            if ui.add_enabled(!in_anki, egui::Button::new(label)).clicked() {
                                anki = true;
                            }
                            if ui.button(t(Tr::CopyWord)).clicked() {
                                ui.ctx().copy_text(p.word.clone());
                            }
                        });
                    });
            });
        if save || anki {
            let id = self.save_popup_word();
            if let (true, Some(id)) = (anki, id) {
                self.export_to_anki(id);
            }
        }
        if close {
            self.close_popup();
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.settings_draft.take() else {
            return;
        };
        let (mut open, mut save, mut cancel) = (true, false, false);
        let (asr_setup, mut asr_action) = (&self.asr_setup, None);
        let (tts_setup, mut tts_action) = (&self.tts_setup, None);
        let (search_ui, mut check_sources) = (&self.search_ui, false);
        egui::Window::new(t(Tr::SettingsTitle))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(dialog_height(ctx).min(520.0))
                    .show(ui, |ui| {
                        let wide = 300.0;
                        let section = |ui: &mut egui::Ui, title: &str| {
                            ui.add_space(8.0);
                            ui.label(RichText::new(title).strong().color(ACCENT));
                        };
                        section(ui, t(Tr::SectionLanguages));
                        egui::Grid::new("lang")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(t(Tr::InterfaceLanguage));
                                egui::ComboBox::from_id_salt("ui-lang")
                                    .selected_text(ui_lang_name(&draft.general.ui_lang))
                                    .show_ui(ui, |ui| {
                                        for code in ["auto", "en", "ru", "uk"] {
                                            ui.selectable_value(
                                                &mut draft.general.ui_lang,
                                                code.to_owned(),
                                                ui_lang_name(code),
                                            );
                                        }
                                    });
                                ui.end_row();
                                ui.label(t(Tr::Learning));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.general.learn_lang)
                                        .hint_text(t(Tr::LearnHint))
                                        .desired_width(wide),
                                );
                                ui.end_row();
                                ui.label(t(Tr::TranslateInto));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.general.native_lang)
                                        .hint_text("ru, uk, en…")
                                        .desired_width(wide),
                                );
                                ui.end_row();
                            });
                        section(ui, t(Tr::SectionSubtitles));
                        egui::Grid::new("subs")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(t(Tr::TextSize));
                                ui.add(egui::Slider::new(
                                    &mut draft.subtitles.font_size,
                                    16.0..=60.0,
                                ));
                                ui.end_row();
                                ui.label(t(Tr::TranslationSize));
                                ui.add(egui::Slider::new(
                                    &mut draft.subtitles.secondary_scale,
                                    0.5..=1.2,
                                ));
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(
                                    &mut draft.subtitles.auto_translate,
                                    t(Tr::AutoTranslateCheck),
                                );
                                ui.end_row();
                            });
                        section(ui, t(Tr::SectionTranslation));
                        egui::Grid::new("tr")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(t(Tr::ServiceRow));
                                egui::ComboBox::from_id_salt("provider")
                                    .selected_text(provider_name(&draft.translation.provider))
                                    .show_ui(ui, |ui| {
                                        for id in ["google", "deepl", "openai", "anthropic"] {
                                            ui.selectable_value(
                                                &mut draft.translation.provider,
                                                id.to_owned(),
                                                provider_name(id),
                                            );
                                        }
                                    });
                                ui.end_row();
                                let llm = matches!(
                                    draft.translation.provider.as_str(),
                                    "openai" | "anthropic"
                                );
                                if llm {
                                    ui.label(t(Tr::ModelRow));
                                    let hint = if draft.translation.provider == "anthropic" {
                                        "claude-opus-5-5"
                                    } else {
                                        t(Tr::ModelNameHint)
                                    };
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.translation.model)
                                            .hint_text(hint)
                                            .desired_width(wide),
                                    );
                                    ui.end_row();
                                }
                                if draft.translation.provider == "openai" {
                                    ui.label(t(Tr::ServerAddressRow));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.translation.base_url)
                                            .hint_text(t(Tr::OpenAiAddressHint))
                                            .desired_width(wide),
                                    );
                                    ui.end_row();
                                }
                                if draft.translation.provider != "google" {
                                    ui.label(t(Tr::ApiKeyRow));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.translation.api_key)
                                            .password(true)
                                            .desired_width(wide),
                                    );
                                    ui.end_row();
                                }
                            });
                        section(ui, t(Tr::SectionSpeechRecognition));
                        asr_action = crate::setup_ui::settings_section(ui, &mut draft, asr_setup);
                        ui.label(RichText::new(t(Tr::Advanced)).color(MUTED));
                        egui::Grid::new("asr")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label("whisper-server");
                                path_field(ui, &mut draft.asr.server_path, wide);
                                ui.end_row();
                                ui.label(t(Tr::ModelFileRow));
                                path_field(ui, &mut draft.asr.model_path, wide);
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(&mut draft.asr.enabled, t(Tr::AsrAutoCheck));
                                ui.end_row();
                            });
                        section(ui, t(Tr::SectionVoiceOver));
                        tts_action = crate::tts_ui::settings_section(ui, &mut draft, tts_setup);
                        section(ui, "Anki");
                        egui::Grid::new("anki")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(t(Tr::DeckRow));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.anki.deck)
                                        .desired_width(wide),
                                );
                                ui.end_row();
                                ui.label(t(Tr::AnkiAddressRow));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.anki.url)
                                        .desired_width(wide),
                                );
                                ui.end_row();
                            });
                        section(ui, t(Tr::SectionSources));
                        check_sources =
                            crate::search_ui::sources_section(ui, &mut draft, search_ui, wide);
                        section(ui, t(Tr::SectionPlayer));
                        egui::Grid::new("player")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(t(Tr::HwdecRow));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.player.hwdec)
                                        .hint_text("auto")
                                        .desired_width(wide),
                                );
                                ui.end_row();
                                ui.label("ffmpeg");
                                path_field(ui, &mut draft.player.ffmpeg_path, wide);
                                ui.end_row();
                                ui.label(t(Tr::InterfaceScaleRow));
                                ui.add(egui::Slider::new(&mut draft.general.ui_scale, 0.75..=2.0));
                                ui.end_row();
                                if crate::shell::SUPPORTED {
                                    ui.label("");
                                    ui.checkbox(
                                        &mut draft.general.shell_integration,
                                        t(Tr::ShellCheck),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new(t(Tr::SaveButton)).strong())
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button(t(Tr::Cancel)).clicked() {
                        cancel = true;
                    }
                });
            });
        self.asr_setup.scroll_to = false;
        self.tts_setup.scroll_to = false;
        self.search_ui.scroll_to = false;
        if check_sources {
            self.check_sources(draft.sources.clone());
        }
        if let Some(action) = asr_action {
            self.asr_settings_action(action, &draft);
        }
        if let Some(action) = tts_action {
            self.tts_settings_action(action, &draft);
        }
        if save {
            self.apply_settings(draft);
        } else if open && !cancel {
            self.settings_draft = Some(draft);
        }
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        let Some((text, _, _)) = &self.status else {
            return;
        };
        if crate::control::controls_override() == Some(false) {
            return;
        }
        egui::Area::new(Id::unique("toast"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 56.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(RAISED)
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(text);
                    });
            });
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

fn provider_name(id: &str) -> &'static str {
    match id {
        "google" => t(Tr::ProviderGoogle),
        "deepl" => "DeepL",
        "openai" => t(Tr::ProviderOpenAi),
        "anthropic" => "Claude",
        _ => t(Tr::ProviderUnknown),
    }
}

/// Each language is named in itself, so it can be found whatever the interface says.
fn ui_lang_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "ru" => "Русский",
        "uk" => "Українська",
        _ => t(Tr::LangAuto),
    }
}

fn path_field(ui: &mut egui::Ui, value: &mut String, width: f32) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(t(Tr::PathAutoHint))
                .desired_width(width - 70.0),
        );
        if ui.button(t(Tr::BrowseButton)).clicked()
            && let Some(path) = rfd::FileDialog::new().pick_file()
        {
            *value = path.to_string_lossy().into_owned();
        }
    });
}

/// Subtitle text with a dark outline, readable on any picture.
fn outlined(
    painter: &egui::Painter,
    at: egui::Pos2,
    galley: Arc<egui::Galley>,
    color: Color32,
    scale: f32,
) {
    let d = (1.6 * scale).max(1.0);
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
        painter.galley_with_override_text_color(
            at + vec2(dx, dy),
            galley.clone(),
            Color32::from_black_alpha(230),
        );
    }
    painter.galley(at, galley, color);
}

/// How tall the scrolling part of a dialog may be, so its buttons stay inside a small window.
pub fn dialog_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 110.0).clamp(120.0, 700.0)
}
