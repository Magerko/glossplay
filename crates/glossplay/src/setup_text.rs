//! The texts of the component installer and the voice-over that need more than a lookup:
//! sizes, status lines and the like. The plain texts live in `i18n`.

use crate::i18n::{Tr, decimal, t, tf};
use gp_ai::download::DownloadError;
use std::path::Path;

/// "685 MB", "1.62 GB": decimal units, as download sites show them.
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    let mb = bytes as f64 / MB;
    let gb = mb / 1000.0;
    if gb >= 100.0 {
        tf(Tr::UnitGb, &[&format!("{gb:.0}")])
    } else if gb >= 10.0 {
        tf(Tr::UnitGb, &[&decimal(format!("{gb:.1}"))])
    } else if gb >= 1.0 {
        tf(Tr::UnitGb, &[&decimal(format!("{gb:.2}"))])
    } else if mb >= 10.0 {
        tf(Tr::UnitMb, &[&format!("{mb:.0}")])
    } else {
        tf(Tr::UnitMb, &[&decimal(format!("{mb:.1}"))])
    }
}

pub fn speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return String::new();
    }
    tf(Tr::PerSecond, &[&size(bytes_per_sec as u64)])
}

// Installer errors.

pub fn cannot_write(path: &Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::StorageFull {
        return tf(Tr::DiskFull, &[&path.display()]);
    }
    tf(Tr::CannotWrite, &[&path.display(), e])
}

pub fn cannot_read(path: &Path, e: &std::io::Error) -> String {
    tf(Tr::CannotRead, &[&path.display(), e])
}

pub fn cannot_remove(e: &std::io::Error) -> String {
    tf(Tr::CannotRemove, &[e])
}

pub fn in_use(path: &Path, e: &std::io::Error) -> String {
    tf(Tr::InUse, &[&path.display(), e])
}

pub fn not_enough_space(needed: u64, free: u64, at: &Path) -> String {
    tf(
        Tr::NotEnoughSpace,
        &[&size(needed), &at.display(), &size(free)],
    )
}

pub fn download_failed(label: &str, e: &DownloadError) -> String {
    tf(Tr::DownloadFailed, &[&label, e])
}

pub fn damaged_download(label: &str) -> String {
    tf(Tr::DamagedDownload, &[&label])
}

pub fn bad_archive(path: &Path, e: &dyn std::fmt::Display) -> String {
    tf(Tr::BadArchive, &[&path.display(), &e])
}

pub fn archive_lacks(path: &Path, missing: &[&str]) -> String {
    if missing.is_empty() {
        tf(Tr::ArchiveEmpty, &[&path.display()])
    } else {
        tf(Tr::ArchiveLacks, &[&path.display(), &missing.join(", ")])
    }
}

// Speech recognition setup.

pub fn model_label(model: &str) -> String {
    tf(Tr::ModelLabel, &[&model])
}

pub fn engine_auto_is(engine: &str) -> String {
    // Inside the brackets a Russian or Ukrainian name starts with a small letter.
    let mut chars = engine.chars();
    let engine = match (crate::i18n::lang(), chars.next()) {
        (crate::i18n::Lang::En, _) | (_, None) => engine.to_owned(),
        (_, Some(first)) => first.to_lowercase().chain(chars).collect(),
    };
    tf(Tr::EngineAutoIs, &[&t(Tr::EngineAuto), &engine])
}

pub fn model_hint(model: &str) -> &'static str {
    match model {
        "large-v3-turbo" => t(Tr::ModelHintTurbo),
        "large-v3-turbo-q5_0" => t(Tr::ModelHintTurboQ5),
        "small" => t(Tr::ModelHintSmall),
        "base" => t(Tr::ModelHintBase),
        _ => "",
    }
}

pub fn space_line(installed: u64, needed: u64, free: Option<u64>) -> String {
    match free {
        Some(free) => tf(
            Tr::SpaceLineFree,
            &[&size(installed), &size(needed), &size(free)],
        ),
        None => tf(Tr::SpaceLine, &[&size(installed), &size(needed)]),
    }
}

pub fn gpu_line(name: &str, vram_mb: u64) -> String {
    let gb = decimal(format!("{:.0}", vram_mb as f64 / 1024.0));
    tf(Tr::GpuLine, &[&name, &gb])
}

pub fn installed_status(build: &str, model: &str, bytes: u64) -> String {
    tf(Tr::InstalledStatus, &[&build, &model, &size(bytes)])
}

pub fn installed_models_only(model: &str) -> String {
    tf(Tr::InstalledModelsOnly, &[&model])
}

pub fn stage(stage: crate::components::Stage, file: &str) -> String {
    use crate::components::Stage;
    match stage {
        Stage::Preparing => t(Tr::StagePreparing).to_owned(),
        Stage::Downloading => tf(Tr::StageDownloading, &[&file]),
        Stage::Verifying => tf(Tr::StageVerifying, &[&file]),
        Stage::Unpacking => tf(Tr::StageUnpacking, &[&file]),
        Stage::Finishing => t(Tr::StageFinishing).to_owned(),
    }
}

pub fn percent_line(fraction: f32, bytes_per_sec: f64) -> String {
    let speed = speed(bytes_per_sec);
    if speed.is_empty() {
        format!("{:.0}%", fraction * 100.0)
    } else {
        format!("{:.0}% · {speed}", fraction * 100.0)
    }
}

pub fn file_line(stage_text: &str, done: u64, total: u64) -> String {
    if total == 0 {
        stage_text.to_owned()
    } else {
        tf(Tr::FileLine, &[&stage_text, &size(done), &size(total)])
    }
}

// Voice-over.

pub const TTS_LINK_CC: &str = "CC BY-NC 4.0";
pub const TTS_URL_CC: &str = "https://creativecommons.org/licenses/by-nc/4.0/";
pub const TTS_LINK_BOSON: &str = "Boson Higgs Audio 2 Community License";
pub const TTS_URL_BOSON: &str =
    "https://huggingface.co/bosonai/higgs-audio-v2-tokenizer/blob/main/LICENSE";
pub const TTS_LINK_MODEL: &str = "OmniVoice model card";
pub const TTS_URL_MODEL: &str = "https://huggingface.co/k2-fsa/OmniVoice";
pub const HINT_TTS_MODEL: &str = "omnivoice";

pub fn tts_installed_status(build: &str, bytes: u64) -> String {
    tf(Tr::TtsInstalledStatus, &[&build, &size(bytes)])
}

pub fn tts_cli_plan(build: &str, download: u64, dir: &Path) -> String {
    format!(
        "Installing the voice-over: {build} + OmniVoice q8_0, {} to download, into {}",
        size(download),
        dir.display()
    )
}

pub fn tts_language(lang: &str) -> String {
    tf(Tr::TtsLanguage, &[&lang])
}

pub fn dub_stopped(e: &str) -> String {
    tf(Tr::DubStopped, &[&e])
}

pub fn dub_line_failed(e: &str) -> String {
    tf(Tr::DubLineFailed, &[&e])
}

pub fn dub_engine_quit(times: usize) -> String {
    tf(Tr::DubEngineQuit, &[&times])
}

pub fn dub_cannot_start(e: &dyn std::fmt::Display) -> String {
    tf(Tr::DubCannotStart, &[&e])
}

// Command line; these stay in English.

pub const TTS_CLI_BAD_ENGINE: &str = "--tts-variant must be vulkan or nvidia";

pub const CLI_BAD_ENGINE: &str = "--asr-variant must be auto, nvidia or cpu";

pub fn cli_bad_model(known: &[&str]) -> String {
    format!("--asr-model must be one of: {}", known.join(", "))
}

pub fn resuming(file: &str, have: u64, total: u64) -> String {
    if have >= total {
        format!("{file}: already downloaded")
    } else {
        format!(
            "{file}: continuing an interrupted download from {} of {}",
            size(have),
            size(total)
        )
    }
}

pub fn cli_plan(build: &str, model: &str, download: u64, dir: &Path) -> String {
    format!(
        "Installing speech recognition: {build} + {model}, {} to download, into {}",
        size(download),
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_download_pages() {
        assert_eq!(size(684_913_404), "685 MB");
        assert_eq!(size(1_624_555_275), "1.62 GB");
        assert_eq!(size(885_098), "0.9 MB");
        assert_eq!(size(8_928_640), "8.9 MB");
        assert_eq!(size(209_770_000_000), "210 GB");
        assert_eq!(size(23_400_000_000), "23.4 GB");
        assert_eq!(speed(0.0), "");
        assert_eq!(speed(12_300_000.0), "12 MB/s");
    }
}
