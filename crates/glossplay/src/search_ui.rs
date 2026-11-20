//! Search in the user's own sources: their Torznab server finds torrents, their TorrServer
//! streams the chosen file, and the player opens that stream like any other video.

use crate::app::App;
use crate::i18n::{Tr, t, tf};
use crate::jobs::Msg;
use crate::session::{StreamSub, sidecar_lang};
use crate::setup_text;
use crate::theme::{ACCENT, DANGER, MUTED, RAISED, TEXT};
use eframe::egui::{self, Align2, Color32, FontId, Id, Key, Rect, RichText, Sense, pos2, vec2};
use gp_ai::AiError;
use gp_ai::torrserver::{self, Source, Torrent, TorrentFile};
use gp_ai::torznab::{self, Release, Resolved};
use gp_core::config::{Config, Sources};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_RESULTS: usize = 200;
const VIDEO_EXTENSIONS: [&str; 7] = ["mkv", "mp4", "avi", "mov", "webm", "ts", "m4v"];
const ROW: f32 = 26.0;

pub enum View {
    Results,
    Searching,
    Failed(String),
    Opening,
    Files {
        torrent: Torrent,
        videos: Vec<TorrentFile>,
    },
}

pub enum Check {
    Idle,
    Running,
    Done(Vec<Result<String, String>>),
}

pub struct SearchUi {
    pub open: bool,
    pub query: String,
    pub results: Vec<Release>,
    /// A search has come back, so an empty list means nothing was found.
    pub searched: bool,
    pub view: View,
    pub selected: usize,
    focus_query: bool,
    /// Results came in: Enter now opens the selected row instead of searching again.
    blur_query: bool,
    scroll_to_selected: bool,
    /// The request whose answer is awaited. An older answer is dropped, and a worker still
    /// waiting for a torrent stops when it sees this change.
    pub current: Arc<AtomicU64>,
    opening_title: String,
    pub check: Check,
    /// Scroll Settings to the Sources section (self-test screenshots).
    pub scroll_to: bool,
    /// A torrent I added to TorrServer for what plays now; removed once another torrent is
    /// played or the player closes. One the server had before is never mine to remove.
    added: Option<(String, String)>,
}

impl SearchUi {
    pub fn new() -> SearchUi {
        SearchUi {
            open: false,
            query: String::new(),
            results: Vec::new(),
            searched: false,
            view: View::Results,
            selected: 0,
            focus_query: false,
            blur_query: false,
            scroll_to_selected: false,
            current: Arc::default(),
            opening_title: String::new(),
            check: Check::Idle,
            scroll_to: false,
            added: None,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.view, View::Searching | View::Opening)
    }

    fn next_request(&self) -> u64 {
        self.current.fetch_add(1, Ordering::SeqCst) + 1
    }
}

pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The video files of a torrent, in name order, so the episodes of a season line up.
pub fn video_files(files: &[TorrentFile]) -> Vec<TorrentFile> {
    let mut videos: Vec<TorrentFile> = files
        .iter()
        .filter(|f| is_video(&f.path))
        .cloned()
        .collect();
    videos.sort_by_key(|f| f.path.to_lowercase());
    videos
}

/// Subtitle files of the torrent named after `video`, as streams of the same server.
pub fn stream_subs(
    base: &str,
    hash: &str,
    files: &[TorrentFile],
    video: &TorrentFile,
) -> Vec<StreamSub> {
    let name = video.name();
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let mut subs: Vec<StreamSub> = files
        .iter()
        .filter_map(|f| {
            let lang = sidecar_lang(stem, f.name())?;
            Some(StreamSub {
                url: torrserver::stream_url(base, hash, f.index, f.name()),
                name: f.name().to_owned(),
                lang,
            })
        })
        .collect();
    subs.sort_by(|a, b| a.name.cmp(&b.name));
    subs
}

/// The endpoint and key to use; a key in the environment or in the address counts too.
fn endpoint_and_key(s: &Sources) -> (String, String) {
    let (endpoint, in_url) = torznab::endpoint(&s.torznab_url);
    let key = torznab::resolve_key(&s.torznab_key, &|n| std::env::var(n).ok());
    (
        endpoint,
        if key.is_empty() {
            in_url.unwrap_or_default()
        } else {
            key
        },
    )
}

pub fn search_error(e: AiError) -> String {
    match e {
        AiError::Network(e) => tf(Tr::SearchUnreachable, &[&e]),
        AiError::BadKey => t(Tr::SearchBadKey).to_owned(),
        AiError::Http {
            status: status @ (404 | 405),
            ..
        } => tf(Tr::SearchWrongAddress, &[&status]),
        other => tf(Tr::SearchFailed, &[&other]),
    }
}

pub fn torrserver_error(e: AiError) -> String {
    match e {
        AiError::Network(e) => tf(Tr::TorrServerUnreachable, &[&e]),
        AiError::TimedOut => t(Tr::NoMetadata).to_owned(),
        other => tf(Tr::TorrServerFailed, &[&other]),
    }
}

fn run_search(sources: &Sources, query: &str) -> Result<Vec<Release>, String> {
    let (endpoint, key) = endpoint_and_key(sources);
    torznab::search(&endpoint, &key, query, MAX_RESULTS).map_err(search_error)
}

/// Turns a release into a torrent TorrServer knows, and waits for its list of files.
fn list_release(
    sources: &Sources,
    link: &torznab::Link,
    cancelled: &dyn Fn() -> bool,
) -> Result<Torrent, String> {
    let base = torrserver::base(&sources.torrserver_url);
    let resolved = torznab::resolve(link).map_err(|e| tf(Tr::LinkFailed, &[&e]))?;
    let source = match &resolved {
        Resolved::Magnet(m) | Resolved::Url(m) => Source::Link(m),
        Resolved::Torrent(bytes) => Source::File(bytes),
    };
    torrserver::files_with(&base, source, torrserver::FILES_TIMEOUT, cancelled)
        .map_err(torrserver_error)
}

fn check(sources: &Sources) -> Vec<Result<String, String>> {
    let search = if sources.torznab_url.trim().is_empty() {
        Err(tf(Tr::CheckSearchFailed, &[&t(Tr::CheckNotSet)]))
    } else {
        let (endpoint, key) = endpoint_and_key(sources);
        torznab::check(&endpoint, &key)
            .map(|n| tf(Tr::CheckSearchOk, &[&n]))
            .map_err(|e| tf(Tr::CheckSearchFailed, &[&search_error(e)]))
    };
    let base = torrserver::base(&sources.torrserver_url);
    let ts = if base.is_empty() {
        Err(tf(Tr::CheckTorrServerFailed, &[&t(Tr::CheckNotSet)]))
    } else {
        torrserver::version(&base)
            .map(|v| tf(Tr::CheckTorrServerOk, &[&v]))
            .map_err(|e| tf(Tr::CheckTorrServerFailed, &[&torrserver_error(e)]))
    };
    vec![search, ts]
}

fn forget(base: String, hash: String) {
    std::thread::spawn(move || {
        let _ = torrserver::remove(&base, &hash);
    });
}

enum Action {
    Search,
    Open(usize),
    Play(usize),
    Back,
}

impl App {
    pub fn search_available(&self) -> bool {
        self.cfg.sources.search_ready()
    }

    /// The torrent and file of a stream of the configured TorrServer.
    pub fn torrserver_stream(&self, media: &str) -> Option<(String, u32)> {
        torrserver::parse_stream_url(&self.cfg.sources.torrserver_url, media)
    }

    pub fn open_search(&mut self, query: Option<String>) {
        if !self.search_available() {
            return;
        }
        self.close_popup();
        let s = &mut self.search_ui;
        s.open = true;
        s.focus_query = true;
        if let Some(q) = query.filter(|q| !q.trim().is_empty()) {
            s.query = q;
            self.start_search();
        }
    }

    pub fn close_search(&mut self) {
        let s = &mut self.search_ui;
        s.open = false;
        s.next_request();
        if s.busy() {
            s.view = View::Results;
        }
    }

    pub fn start_search(&mut self) {
        let query = self.search_ui.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let request = self.search_ui.next_request();
        self.search_ui.view = View::Searching;
        let sources = self.cfg.sources.clone();
        self.jobs.spawn(move || Msg::SearchDone {
            request,
            result: run_search(&sources, &query),
        });
    }

    fn open_release(&mut self, index: usize) {
        let Some(r) = self.search_ui.results.get(index).cloned() else {
            return;
        };
        let request = self.search_ui.next_request();
        self.search_ui.view = View::Opening;
        self.search_ui.opening_title = r.title.clone();
        let (sources, current) = (self.cfg.sources.clone(), self.search_ui.current.clone());
        self.jobs.spawn(move || {
            let cancelled = || current.load(Ordering::SeqCst) != request;
            Msg::FilesListed {
                request,
                result: list_release(&sources, &r.link, &cancelled),
            }
        });
    }

    fn play_file(&mut self, torrent: &Torrent, file: &TorrentFile, title: String) {
        let base = torrserver::base(&self.cfg.sources.torrserver_url);
        let url = torrserver::stream_url(&base, &torrent.hash, file.index, file.name());
        let subs = stream_subs(&base, &torrent.hash, &torrent.files, file);
        if let Some((b, h)) = self.search_ui.added.take() {
            if h == torrent.hash {
                self.search_ui.added = Some((b, h));
            } else {
                forget(b, h);
            }
        }
        if !torrent.was_there && self.search_ui.added.is_none() {
            self.search_ui.added = Some((base, torrent.hash.clone()));
        }
        self.search_ui.open = false;
        self.search_ui.view = View::Results;
        self.open_with(&url, Some(title), Some(subs));
    }

    /// Removes the torrent I added, waiting a moment for it on the way out.
    pub fn forget_added_torrent(&mut self) {
        let Some((base, hash)) = self.search_ui.added.take() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = torrserver::remove(&base, &hash);
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
    }

    pub fn check_sources(&mut self, sources: Sources) {
        self.search_ui.check = Check::Running;
        self.jobs
            .spawn(move || Msg::SourcesChecked(check(&sources)));
    }

    /// Takes the answers of search jobs; returns the message back when it is not one.
    pub fn on_search_msg(&mut self, msg: Msg) -> Option<Msg> {
        let current = self.search_ui.current.load(Ordering::SeqCst);
        match msg {
            Msg::SearchDone { request, result } if request == current => {
                let s = &mut self.search_ui;
                s.searched = true;
                match result {
                    Ok(found) => {
                        s.results = found;
                        s.view = View::Results;
                        s.selected = 0;
                        s.scroll_to_selected = true;
                        s.blur_query = !s.results.is_empty();
                    }
                    Err(e) => s.view = View::Failed(e),
                }
            }
            Msg::SearchDone { .. } => {}
            Msg::FilesListed { request, result } => match result {
                Ok(torrent) if request != current || !self.search_ui.open => {
                    if !torrent.was_there {
                        forget(
                            torrserver::base(&self.cfg.sources.torrserver_url),
                            torrent.hash,
                        );
                    }
                }
                Ok(torrent) => {
                    let videos = video_files(&torrent.files);
                    match videos.len() {
                        0 => {
                            if !torrent.was_there {
                                forget(
                                    torrserver::base(&self.cfg.sources.torrserver_url),
                                    torrent.hash,
                                );
                            }
                            self.search_ui.view = View::Failed(t(Tr::NoVideoFiles).into());
                        }
                        1 => {
                            let title = self.search_ui.opening_title.clone();
                            self.play_file(&torrent, &videos[0], title);
                        }
                        _ => {
                            self.search_ui.selected = 0;
                            self.search_ui.scroll_to_selected = true;
                            self.search_ui.view = View::Files { torrent, videos };
                        }
                    }
                }
                Err(e) if request == current => self.search_ui.view = View::Failed(e),
                Err(_) => {}
            },
            Msg::SourcesChecked(lines) => self.search_ui.check = Check::Done(lines),
            other => return Some(other),
        }
        None
    }

    pub fn search_window(&mut self, ctx: &egui::Context) {
        if !self.search_ui.open {
            return;
        }
        let screen = ctx.content_rect();
        let width = (screen.width() - 60.0).clamp(400.0, 920.0);
        let height = crate::ui::dialog_height(ctx).min(520.0);
        let mut open = true;
        let mut action = None;
        egui::Window::new(t(Tr::SearchTitle))
            .id(Id::unique("search-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .fixed_size(vec2(width, height))
            .show(ctx, |ui| action = search_contents(&mut self.search_ui, ui));
        if !open {
            return self.close_search();
        }
        match action {
            Some(Action::Search) => self.start_search(),
            Some(Action::Open(i)) => self.open_release(i),
            Some(Action::Play(i)) => {
                if let View::Files { torrent, videos } = &self.search_ui.view
                    && let Some(file) = videos.get(i)
                {
                    let (torrent, file) = (torrent.clone(), file.clone());
                    let name = file.name();
                    let title = name.rsplit_once('.').map_or(name, |(s, _)| s).to_owned();
                    self.play_file(&torrent, &file, title);
                }
            }
            Some(Action::Back) => {
                self.search_ui.view = View::Results;
                self.search_ui.scroll_to_selected = true;
            }
            None => {}
        }
    }
}

fn search_contents(s: &mut SearchUi, ui: &mut egui::Ui) -> Option<Action> {
    let mut action = None;
    let mut query_active = false;
    ui.horizontal(|ui| {
        let field = egui::TextEdit::singleline(&mut s.query)
            .hint_text(t(Tr::SearchField))
            .desired_width(ui.available_width() - 90.0);
        let resp = ui.add(field);
        if s.focus_query {
            resp.request_focus();
            s.focus_query = false;
        }
        if s.blur_query {
            resp.surrender_focus();
            s.blur_query = false;
        }
        query_active = resp.has_focus() || resp.lost_focus();
        let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if (ui.button(t(Tr::SearchButton)).clicked() || entered) && !s.query.trim().is_empty() {
            action = Some(Action::Search);
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            resp.surrender_focus();
            s.selected = 0;
            s.scroll_to_selected = true;
        }
    });
    ui.add_space(6.0);
    let rows = match &s.view {
        View::Results => s.results.len(),
        View::Files { videos, .. } => videos.len(),
        _ => 0,
    };
    if !query_active && rows > 0 {
        let (up, down, enter) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Enter),
            )
        });
        if up || down {
            s.selected = if up {
                s.selected.saturating_sub(1)
            } else {
                (s.selected + 1).min(rows - 1)
            };
            s.scroll_to_selected = true;
        }
        if enter && action.is_none() {
            action = Some(match s.view {
                View::Files { .. } => Action::Play(s.selected.min(rows - 1)),
                _ => Action::Open(s.selected.min(rows - 1)),
            });
        }
    }
    match &s.view {
        View::Searching => busy(ui, t(Tr::Searching), &s.query),
        View::Opening => busy(ui, t(Tr::GettingFiles), &s.opening_title),
        View::Failed(e) => {
            ui.label(RichText::new(e).color(DANGER));
            if !s.results.is_empty() && ui.button(t(Tr::BackToResults)).clicked() {
                action = Some(Action::Back);
            }
        }
        View::Results if s.results.is_empty() => {
            if s.searched {
                ui.label(RichText::new(t(Tr::NothingFound)).color(MUTED));
            }
        }
        View::Results => {
            ui.label(
                RichText::new(tf(Tr::FoundCount, &[&s.results.len()]))
                    .color(MUTED)
                    .small(),
            );
            let cells: Vec<[String; 5]> = s
                .results
                .iter()
                .map(|r| {
                    [
                        r.title.clone(),
                        r.size.map_or("—".into(), setup_text::size),
                        match (r.seeders, r.peers) {
                            (Some(a), Some(b)) => format!("{a} / {b}"),
                            (Some(a), None) => a.to_string(),
                            _ => "—".into(),
                        },
                        r.published.clone().unwrap_or_default(),
                        r.indexer.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            let headers = [
                t(Tr::ColTitle),
                t(Tr::ColSize),
                t(Tr::ColPeers),
                t(Tr::ColDate),
                t(Tr::ColSource),
            ];
            if let Some((i, double)) = table(
                ui,
                s.selected,
                &mut s.scroll_to_selected,
                &headers,
                &[0.0, 84.0, 100.0, 92.0, 150.0],
                &cells,
            ) {
                s.selected = i;
                if double {
                    action = Some(Action::Open(i));
                }
            }
        }
        View::Files { torrent, videos } => {
            ui.horizontal(|ui| {
                if ui.button(t(Tr::BackToResults)).clicked() {
                    action = Some(Action::Back);
                }
                let name = if torrent.name.is_empty() {
                    &s.opening_title
                } else {
                    &torrent.name
                };
                ui.label(RichText::new(name).strong());
            });
            ui.label(
                RichText::new(tf(Tr::ChooseFile, &[&videos.len()]))
                    .color(MUTED)
                    .small(),
            );
            let cells: Vec<[String; 2]> = videos
                .iter()
                .map(|f| [f.path.clone(), setup_text::size(f.size)])
                .collect();
            let headers = [t(Tr::ColTitle), t(Tr::ColSize)];
            if let Some((i, double)) = table(
                ui,
                s.selected,
                &mut s.scroll_to_selected,
                &headers,
                &[0.0, 90.0],
                &cells,
            ) {
                s.selected = i;
                if double {
                    action = Some(Action::Play(i));
                }
            }
        }
    }
    action
}

fn busy(ui: &mut egui::Ui, what: &str, about: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(what);
    });
    ui.label(RichText::new(about).color(MUTED).small());
}

/// A list with columns; `widths` of 0 take the room left. Returns the row clicked and
/// whether it was a double click.
fn table<const N: usize>(
    ui: &mut egui::Ui,
    selected: usize,
    scroll_to: &mut bool,
    headers: &[&str; N],
    widths: &[f32; N],
    cells: &[[String; N]],
) -> Option<(usize, bool)> {
    const GAP: f32 = 10.0;
    let full = ui.available_width();
    let fixed: f32 = widths.iter().sum::<f32>() + GAP * (N as f32 + 1.0);
    let flex = (full - fixed).max(120.0);
    let columns: Vec<(f32, f32)> = {
        let mut x = GAP;
        widths
            .iter()
            .map(|&w| {
                let w = if w == 0.0 { flex } else { w };
                let col = (x, w);
                x += w + GAP;
                col
            })
            .collect()
    };
    let draw = |ui: &egui::Ui, rect: Rect, texts: &[&str], font: FontId, color: Color32| {
        for ((x, w), text) in columns.iter().zip(texts) {
            let mut job = egui::text::LayoutJob::simple_singleline(
                text.replace('\n', " "),
                font.clone(),
                color,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(*w);
            let galley = ui.painter().layout_job(job);
            let y = rect.center().y - galley.size().y / 2.0;
            ui.painter().galley(pos2(rect.left() + x, y), galley, color);
        }
    };
    let (head, _) = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover());
    draw(ui, head, headers, FontId::proportional(12.0), MUTED);
    let spacing = ui.spacing().item_spacing.y;
    let mut area = egui::ScrollArea::vertical().auto_shrink(false);
    if *scroll_to {
        *scroll_to = false;
        let offset = selected as f32 * (ROW + spacing) - ui.available_height() / 2.0;
        area = area.vertical_scroll_offset(offset.max(0.0));
    }
    let mut hit = None;
    area.show_rows(ui, ROW, cells.len(), |ui, range| {
        for i in range {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
            if i == selected {
                ui.painter().rect_filled(rect, 5.0, RAISED);
                ui.painter().rect_filled(
                    Rect::from_min_size(rect.min, vec2(3.0, ROW)),
                    2.0,
                    ACCENT,
                );
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(rect, 5.0, RAISED.gamma_multiply(0.6));
            }
            let texts: Vec<&str> = cells[i].iter().map(String::as_str).collect();
            draw(ui, rect, &texts, FontId::proportional(14.0), TEXT);
            let resp = resp.on_hover_text(&cells[i][0]);
            if resp.double_clicked() {
                hit = Some((i, true));
            } else if resp.clicked() {
                hit = Some((i, false));
            }
        }
    });
    hit
}

/// The Sources part of Settings. Returns true when Check was pressed.
pub fn sources_section(ui: &mut egui::Ui, draft: &mut Config, s: &SearchUi, wide: f32) -> bool {
    if s.scroll_to {
        let at = Rect::from_min_size(ui.cursor().min - vec2(0.0, 32.0), vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(egui::Align::TOP));
    }
    ui.add(egui::Label::new(RichText::new(t(Tr::SourcesNote)).color(MUTED).small()).wrap());
    egui::Grid::new("sources")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label(t(Tr::SearchServerRow));
            ui.add(
                egui::TextEdit::singleline(&mut draft.sources.torznab_url)
                    .hint_text("http://…/api")
                    .desired_width(wide),
            );
            ui.end_row();
            ui.label(t(Tr::ApiKeyRow));
            ui.add(
                egui::TextEdit::singleline(&mut draft.sources.torznab_key)
                    .password(true)
                    .desired_width(wide),
            );
            ui.end_row();
            ui.label(t(Tr::TorrServerRow));
            ui.add(
                egui::TextEdit::singleline(&mut draft.sources.torrserver_url)
                    .hint_text("http://…:8090")
                    .desired_width(wide),
            );
            ui.end_row();
        });
    let mut clicked = false;
    ui.horizontal(|ui| {
        let running = matches!(s.check, Check::Running);
        clicked = ui
            .add_enabled(!running, egui::Button::new(t(Tr::CheckButton)))
            .clicked();
        if running {
            ui.spinner();
            ui.label(t(Tr::Checking));
        }
    });
    if let Check::Done(lines) = &s.check {
        for line in lines {
            match line {
                Ok(text) => ui.label(RichText::new(text).color(ACCENT)),
                Err(text) => ui.label(RichText::new(text).color(DANGER)),
            };
        }
    }
    clicked
}

/// `--search QUERY`: the first results, one line each, without links or keys.
pub fn cli_search(query: &str) -> i32 {
    let (cfg, _) = crate::paths::load_config();
    if !cfg.sources.search_ready() {
        eprintln!("set torznab_url and torrserver_url in [sources] of config.toml first");
        return 2;
    }
    match run_search(&cfg.sources, query) {
        Ok(found) => {
            println!("results={}", found.len());
            for r in found.iter().take(10) {
                println!(
                    "release={} | {} | {} | {}",
                    r.seeders.map_or("-".into(), |s| s.to_string()),
                    r.size.map_or("-".into(), setup_text::size),
                    r.indexer.as_deref().unwrap_or("-"),
                    r.title
                );
            }
            i32::from(found.is_empty())
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--stream-url LINK`: adds a torrent to TorrServer and prints its files with stream URLs.
/// The torrent stays on the server, so the URLs can be played; `hash=` says what to remove.
pub fn cli_stream_url(link: &str) -> i32 {
    let (cfg, _) = crate::paths::load_config();
    let base = torrserver::base(&cfg.sources.torrserver_url);
    if base.is_empty() {
        eprintln!("set torrserver_url in [sources] of config.toml first");
        return 2;
    }
    let started = std::time::Instant::now();
    match torrserver::files_with(
        &base,
        Source::Link(link),
        torrserver::FILES_TIMEOUT,
        &|| false,
    ) {
        Ok(torrent) => {
            println!("hash={}", torrent.hash);
            println!("name={}", torrent.name);
            println!("was_there={}", torrent.was_there);
            println!("files_ms={}", started.elapsed().as_millis());
            for f in &torrent.files {
                println!("file={} | {} | {}", f.index, f.size, f.path);
            }
            for f in video_files(&torrent.files) {
                println!(
                    "stream={}",
                    torrserver::stream_url(&base, &torrent.hash, f.index, f.name())
                );
            }
            0
        }
        Err(e) => {
            eprintln!("{}", torrserver_error(e));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(index: u32, path: &str) -> TorrentFile {
        TorrentFile {
            index,
            path: path.into(),
            size: 1,
        }
    }

    #[test]
    fn only_video_files_are_offered_in_name_order() {
        let files = [
            file(1, "Show/Show.S01E02.mkv"),
            file(2, "Show/Show.S01E01.MP4"),
            file(3, "Show/Show.S01E01.en.srt"),
            file(4, "Show/cover.jpg"),
            file(5, "Show/extra.ts"),
        ];
        let v: Vec<u32> = video_files(&files).iter().map(|f| f.index).collect();
        assert_eq!(v, [5, 2, 1]);
        assert!(!is_video("notes.txt") && !is_video("mkv"));
    }

    #[test]
    fn subtitles_named_after_the_video_come_along() {
        let files = [
            file(1, "Sintel/Sintel.de.srt"),
            file(2, "Sintel/Sintel.en.srt"),
            file(6, "Sintel/Sintel.mp4"),
            file(7, "Sintel/Other.en.srt"),
            file(11, "Sintel/poster.jpg"),
        ];
        let subs = stream_subs("http://ts.test:8090", "abc", &files, &files[2]);
        let got: Vec<(&str, Option<&str>)> = subs
            .iter()
            .map(|s| (s.name.as_str(), s.lang.as_deref()))
            .collect();
        assert_eq!(
            got,
            [("Sintel.de.srt", Some("de")), ("Sintel.en.srt", Some("en"))]
        );
        assert!(
            subs[1]
                .url
                .starts_with("http://ts.test:8090/stream/Sintel.en.srt?link=abc&index=2")
        );
    }
}
