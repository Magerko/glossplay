//! The "Words and Anki" tab beyond the two fields: whether Anki answers right now, and the
//! three steps that connect it. Anki is a separate program with an add-on of its own, so
//! "it does not work" needs a plain explanation rather than a silent failure.

use crate::i18n::{Tr, t, tf};
use crate::settings_ui::Form;
use crate::theme;
use eframe::egui::{self, Id, RichText};
use gp_ai::AiError;
use gp_ai::anki::AnkiClient;
use gp_core::config::Config;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The AnkiWeb code of the AnkiConnect add-on; Anki also uses it as the add-on's folder name.
pub const ADDON_CODE: &str = "2055492159";

#[derive(Clone, Debug, PartialEq)]
enum Probe {
    Idle,
    Checking,
    Connected(u32),
    /// Nothing listens at the address; the flag says whether the add-on is on disk.
    NoAnswer(Option<bool>),
    Error(String),
}

#[derive(Clone)]
struct Shared {
    probe: Arc<Mutex<Probe>>,
    copied: Arc<Mutex<bool>>,
}

fn is_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', ':']).next().unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Where Anki keeps its profiles: `ANKI_BASE` if set, else the default of the platform.
fn anki_base() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("ANKI_BASE").filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(base));
    }
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Anki2"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("Anki2")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("Anki2"))
    }
}

fn addon_present_in(base: &Path) -> bool {
    base.join("addons21").join(ADDON_CODE).is_dir()
}

/// Whether the add-on folder exists; `None` when the address is not this computer or the
/// Anki folder cannot be located.
fn addon_on_disk(url: &str) -> Option<bool> {
    if !is_local(url) {
        return None;
    }
    anki_base().map(|base| addon_present_in(&base))
}

fn no_answer_text(on_disk: Option<bool>) -> Tr {
    match on_disk {
        Some(true) => Tr::AnkiStatusNotRunning,
        Some(false) => Tr::AnkiStatusNoAddon,
        None => Tr::AnkiStatusNoAnswer,
    }
}

fn start_probe(ctx: &egui::Context, shared: &Shared, url: &str) {
    *shared.probe.lock().unwrap() = Probe::Checking;
    let slot = shared.probe.clone();
    let ctx = ctx.clone();
    let url = url.trim().to_owned();
    std::thread::spawn(move || {
        let result = match AnkiClient::new(&url).version() {
            Ok(v) => Probe::Connected(v),
            Err(AiError::Network(_)) => Probe::NoAnswer(addon_on_disk(&url)),
            Err(e) => Probe::Error(e.to_string()),
        };
        *slot.lock().unwrap() = result;
        ctx.request_repaint();
    });
}

pub fn section(ui: &mut egui::Ui, form: &Form, draft: &Config) {
    let id = Id::unique("anki-connection");
    let shared = ui.data_mut(|d| {
        d.get_temp_mut_or_insert_with(id, || Shared {
            probe: Arc::new(Mutex::new(Probe::Idle)),
            copied: Arc::new(Mutex::new(false)),
        })
        .clone()
    });
    let probe = shared.probe.lock().unwrap().clone();
    if probe == Probe::Idle {
        start_probe(ui.ctx(), &shared, &draft.anki.url);
    }

    let checking = probe == Probe::Checking || probe == Probe::Idle;
    form.row(ui, t(Tr::AnkiStatusRow), None, |ui| {
        if ui
            .add_enabled(!checking, egui::Button::new(t(Tr::AnkiCheckButton)))
            .clicked()
        {
            start_probe(ui.ctx(), &shared, &draft.anki.url);
        }
    });
    let (text, color) = match &probe {
        Probe::Idle | Probe::Checking => (t(Tr::AnkiChecking).to_owned(), theme::muted()),
        Probe::Connected(v) => (tf(Tr::AnkiConnected, &[v]), theme::accent()),
        Probe::NoAnswer(on_disk) => (t(no_answer_text(*on_disk)).to_owned(), theme::danger()),
        Probe::Error(e) => (tf(Tr::AnkiStatusError, &[e]), theme::danger()),
    };
    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
    ui.add_space(6.0);

    egui::CollapsingHeader::new(RichText::new(t(Tr::AnkiHowTo)).color(theme::muted()))
        .id_salt("anki-how-to")
        .show(ui, |ui| {
            ui.add(egui::Label::new(t(Tr::AnkiStep1)).wrap());
            ui.hyperlink_to("apps.ankiweb.net", "https://apps.ankiweb.net");
            ui.add(egui::Label::new(tf(Tr::AnkiStep2, &[&ADDON_CODE])).wrap());
            ui.add(egui::Label::new(t(Tr::AnkiStep3)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(t(Tr::AnkiCopyCode)).clicked() {
                    ui.ctx().copy_text(ADDON_CODE.to_owned());
                    *shared.copied.lock().unwrap() = true;
                }
                if *shared.copied.lock().unwrap() {
                    ui.label(RichText::new(t(Tr::AnkiCodeCopied)).color(theme::muted()));
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_computer_counts_as_local() {
        assert!(is_local("http://127.0.0.1:8765"));
        assert!(is_local("http://localhost:8765/"));
        assert!(is_local("127.0.0.1:8765"));
        assert!(!is_local("http://192.168.1.5:8765"));
        assert!(!is_local("http://anki.example.com"));
    }

    #[test]
    fn the_add_on_is_found_by_its_folder_name() {
        let base = std::env::temp_dir().join(format!("gp-anki-ui-{}", std::process::id()));
        assert!(!addon_present_in(&base));
        std::fs::create_dir_all(base.join("addons21").join(ADDON_CODE)).unwrap();
        assert!(addon_present_in(&base));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_dead_address_says_what_is_missing() {
        assert_eq!(no_answer_text(Some(true)), Tr::AnkiStatusNotRunning);
        assert_eq!(no_answer_text(Some(false)), Tr::AnkiStatusNoAddon);
        assert_eq!(no_answer_text(None), Tr::AnkiStatusNoAnswer);
        // A remote address cannot be judged by the local folder.
        assert_eq!(addon_on_disk("http://anki.example.com:8765"), None);
    }
}
