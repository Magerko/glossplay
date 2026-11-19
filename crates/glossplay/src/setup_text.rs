//! Every user-facing text of the component installer and the speech recognition setup, in
//! one place so the interface translation can find them.

use gp_ai::download::DownloadError;
use std::path::Path;

/// "685 MB", "1.62 GB": decimal units, as download sites show them.
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    let mb = bytes as f64 / MB;
    let gb = mb / 1000.0;
    if gb >= 100.0 {
        format!("{gb:.0} GB")
    } else if gb >= 10.0 {
        format!("{gb:.1} GB")
    } else if gb >= 1.0 {
        format!("{gb:.2} GB")
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

pub fn speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return String::new();
    }
    format!("{}/s", size(bytes_per_sec as u64))
}

// Installer errors.

pub const CANCELLED: &str =
    "Cancelled. What was downloaded is kept and the next install continues from there.";
pub const NOT_INSTALLED: &str = "This component is not installed";
pub const ALREADY_INSTALLING: &str = "This component is already being installed";
pub const UNSAFE_PATH: &str =
    "The archive contains a file that would land outside the install folder; not installing it";

pub fn cannot_write(path: &Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::StorageFull {
        return format!("The disk is full: could not write {}", path.display());
    }
    format!("Could not write {}: {e}", path.display())
}

pub fn cannot_read(path: &Path, e: &std::io::Error) -> String {
    format!("Could not read {}: {e}", path.display())
}

pub fn cannot_remove(e: &std::io::Error) -> String {
    format!(
        "Could not remove the component: {e}. Close Glossplay and any whisper-server and try again."
    )
}

pub fn in_use(path: &Path, e: &std::io::Error) -> String {
    format!(
        "Could not replace {}: {e}. Another Glossplay window may be using it; close it and try again.",
        path.display()
    )
}

pub fn not_enough_space(needed: u64, free: u64, at: &Path) -> String {
    format!(
        "Not enough disk space: the install needs {} free on the drive of {}, and there is {}.",
        size(needed),
        at.display(),
        size(free)
    )
}

pub fn download_failed(label: &str, e: &DownloadError) -> String {
    format!(
        "Could not download the {label}: {e}. Try again later; the download will continue where it stopped."
    )
}

pub fn damaged_download(label: &str) -> String {
    format!(
        "The downloaded {label} is damaged (its checksum does not match), so it was deleted. Try again."
    )
}

pub fn bad_archive(path: &Path, e: &dyn std::fmt::Display) -> String {
    format!("Could not unpack {}: {e}", path.display())
}

pub fn archive_lacks(path: &Path, missing: &[&str]) -> String {
    if missing.is_empty() {
        format!("{} has none of the expected files", path.display())
    } else {
        format!("{} lacks {}", path.display(), missing.join(", "))
    }
}

// Speech recognition setup.

pub const COMPONENT_NAME: &str = "Speech recognition (Whisper)";
pub const LICENCE_NOTE: &str = "whisper.cpp and the Whisper models are MIT-licensed (ggml-org, OpenAI); the Silero voice detector is MIT-licensed. The NVIDIA build includes NVIDIA CUDA libraries, redistributed under the NVIDIA CUDA licence.";
pub const ENGINE_LABEL_NVIDIA: &str = "whisper.cpp engine for NVIDIA graphics cards";
pub const ENGINE_LABEL_CPU: &str = "whisper.cpp engine for the processor";
pub const VAD_LABEL: &str = "Silero voice detector";

pub fn model_label(model: &str) -> String {
    format!("Whisper model {model}")
}

pub const ENGINE_AUTO: &str = "Automatic";
pub const ENGINE_NVIDIA: &str = "NVIDIA graphics card";
pub const ENGINE_CPU: &str = "Processor (no graphics card)";

pub fn engine_auto_is(engine: &str) -> String {
    format!("{ENGINE_AUTO} ({engine})")
}

pub const BUILD_NVIDIA: &str = "NVIDIA build";
pub const BUILD_CPU: &str = "processor build";
pub const MODEL_AUTO: &str = "Recommended";

pub fn model_hint(model: &str) -> &'static str {
    match model {
        "large-v3-turbo" => "most accurate",
        "large-v3-turbo-q5_0" => "accurate, smaller",
        "small" => "good, works on any computer",
        "base" => "fastest, makes more mistakes",
        _ => "",
    }
}

pub const DIALOG_TITLE: &str = "Speech recognition";
pub const DIALOG_INTRO: &str = "Glossplay can turn speech into subtitles for videos that have none. It runs Whisper on this computer, so nothing is sent anywhere. It needs a one-time download.";
pub const CHECKING_HARDWARE: &str = "Checking the graphics card…";
pub const NO_NVIDIA: &str = "No NVIDIA graphics card found";
pub const NVIDIA_FORCED_WITHOUT_GPU: &str =
    "No NVIDIA graphics card was found; the NVIDIA build will not start without one.";
pub const CPU_SLOW: &str = "Without an NVIDIA graphics card recognition is slower: about as fast as the video plays. Give it a head start before you watch.";
pub const OLD_DRIVER: &str = "The NVIDIA driver is too old for the NVIDIA build (version 525 or newer is needed), so the processor build is suggested. Update the driver to use the graphics card.";
pub const NO_ENGINE_HERE: &str = "There is no ready-made whisper.cpp for this system: install whisper.cpp yourself and set the path to whisper-server under Advanced. The model is downloaded here all the same.";
pub const VC_RUNTIME_MISSING: &str = "whisper.cpp needs the Microsoft Visual C++ runtime, which is not installed. Get it from https://aka.ms/vs/17/release/vc_redist.x64.exe";
pub const ROW_ENGINE: &str = "Engine";
pub const ROW_MODEL: &str = "Model";
pub const ROW_TOTAL: &str = "Download";
pub const ROW_PLACE: &str = "Installs to";
pub const ENGINE_OWN: &str = "your own whisper.cpp";
pub const INSTALL: &str = "Install";
pub const REINSTALL: &str = "Reinstall";
pub const REMOVE: &str = "Remove";
pub const CANCEL: &str = "Cancel";
pub const CLOSE: &str = "Close";
pub const TRY_AGAIN: &str = "Try again";
pub const READY: &str = "Ready. Speech recognition is installed.";
pub const READY_STARTED: &str = "Transcribing this video now.";
pub const READY_HOW: &str =
    "Open a video without subtitles and choose Subtitles → Transcribe speech.";
pub const NOT_INSTALLED_STATUS: &str = "Not installed";
pub const ADVANCED: &str = "Advanced: use my own whisper.cpp";
pub const SET_UP_LINK: &str = "Set up speech recognition for videos without subtitles…";
pub const TOAST_READY: &str = "Speech recognition is installed";
pub const TOAST_REMOVED: &str = "Speech recognition was removed";
pub const NOT_SET_UP: &str =
    "Speech recognition is not set up: choose Subtitles → Transcribe speech to install it";
pub const NO_SERVER: &str =
    "whisper-server was not found: set its path in Settings → Speech recognition → Advanced";
pub const NO_MODEL: &str = "The Whisper model file was not found: set its path in Settings → Speech recognition → Advanced";
pub const FIRST_RUN: &str = " — the first run on this graphics card can take up to a minute";

pub fn space_line(installed: u64, needed: u64, free: Option<u64>) -> String {
    let takes = format!(
        "Takes {} on disk; needs {} free while installing",
        size(installed),
        size(needed)
    );
    match free {
        Some(free) => format!("{takes} ({} free now)", size(free)),
        None => takes,
    }
}

pub fn gpu_line(name: &str, vram_mb: u64) -> String {
    format!("Graphics card: {name}, {:.0} GB", vram_mb as f64 / 1024.0)
}

pub fn installed_status(build: &str, model: &str, bytes: u64) -> String {
    format!("Installed: {build}, {model} ({})", size(bytes))
}

pub fn installed_models_only(model: &str) -> String {
    format!("Installed: model {model} (the engine is your own whisper.cpp)")
}

pub fn stage(stage: crate::components::Stage, file: &str) -> String {
    use crate::components::Stage;
    match stage {
        Stage::Preparing => "Preparing…".to_owned(),
        Stage::Downloading => format!("Downloading {file}"),
        Stage::Verifying => format!("Checking {file}"),
        Stage::Unpacking => format!("Unpacking {file}"),
        Stage::Finishing => "Finishing…".to_owned(),
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
        format!("{stage_text}: {} of {}", size(done), size(total))
    }
}

// Voice-over: the install, the menu, the settings and the status line.

pub const TTS_COMPONENT_NAME: &str = "Voice-over (OmniVoice)";
pub const TTS_LABEL_VULKAN: &str = "audio.cpp voice engine for any graphics card (Vulkan)";
pub const TTS_LABEL_NVIDIA: &str = "audio.cpp voice engine for NVIDIA graphics cards (CUDA)";
pub const TTS_LABEL_CUDA_LIBS: &str = "NVIDIA CUDA libraries for the voice engine";
pub const TTS_LABEL_MODEL: &str = "OmniVoice voice model (q8_0)";
pub const TTS_DIALOG_TITLE: &str = "Voice-over";
pub const TTS_DIALOG_INTRO: &str = "Glossplay can speak the translation over the video, with the original sound turned down. The voice is made on this computer by OmniVoice, so nothing is sent anywhere. It needs a one-time download and a graphics card.";
pub const TTS_SOURCES: &str = "Downloaded from GitHub (0xShug0/audio.cpp, release v0.9.5) and Hugging Face (audio-cpp/audio.cpp-gguf).";
pub const TTS_LICENCE_NC: &str = "Non-commercial use only. The OmniVoice voice model (k2-fsa/OmniVoice) is licensed under CC BY-NC 4.0: neither the model nor what it says may be used for commercial purposes.";
pub const TTS_LICENCE_BOSON: &str = "Built with Higgs Materials licensed from Boson AI USA, Inc. (the audio tokenizer inside the model, under the Boson Higgs Audio 2 Community License).";
pub const TTS_NO_IMPERSONATION: &str =
    "Do not use the voice engine to imitate a real person's voice without their consent.";
pub const TTS_LICENCE_ENGINE: &str = "The audio.cpp engine is Apache-2.0 (ShugoAI). The NVIDIA build includes NVIDIA CUDA libraries, redistributed under the NVIDIA CUDA licence.";
pub const TTS_LINK_CC: &str = "CC BY-NC 4.0";
pub const TTS_URL_CC: &str = "https://creativecommons.org/licenses/by-nc/4.0/";
pub const TTS_LINK_BOSON: &str = "Boson Higgs Audio 2 Community License";
pub const TTS_URL_BOSON: &str =
    "https://huggingface.co/bosonai/higgs-audio-v2-tokenizer/blob/main/LICENSE";
pub const TTS_LINK_MODEL: &str = "OmniVoice model card";
pub const TTS_URL_MODEL: &str = "https://huggingface.co/k2-fsa/OmniVoice";
pub const TTS_AGREE: &str =
    "I will use the voice-over for non-commercial purposes only and accept these licences";
pub const TTS_NEEDS_GPU: &str = "The voice engine needs a graphics card. On the processor alone it is 6–10 times slower than the video plays and cannot keep up.";
pub const TTS_NO_VULKAN: &str = "No Vulkan graphics driver was found on this computer, so the voice engine would run on the processor: 6–10 times slower than the video plays. Install the graphics card driver first.";
pub const TTS_NVIDIA_WITHOUT_GPU: &str =
    "No NVIDIA graphics card was found; the NVIDIA build will not start without one.";
pub const TTS_ENGINE_VULKAN: &str = "Any graphics card (Vulkan)";
pub const TTS_ENGINE_NVIDIA: &str = "NVIDIA graphics card (CUDA): a little faster, much bigger";
pub const TTS_BUILD_VULKAN: &str = "Vulkan build";
pub const TTS_BUILD_NVIDIA: &str = "NVIDIA build";
pub const TTS_READY: &str = "Ready. The voice-over is installed.";
pub const TTS_READY_ON: &str = "The voice-over is on now.";
pub const TTS_READY_HOW: &str = "Turn it on in Subtitles → Voice-over.";
pub const TTS_TOAST_READY: &str = "The voice-over is installed";
pub const TTS_TOAST_REMOVED: &str = "The voice-over was removed";
pub const TTS_NOT_SET_UP: &str =
    "The voice-over is not set up: choose Subtitles → Voice-over to install it";
pub const TTS_NO_ENGINE_HERE: &str = "There is no ready-made voice engine for this system: run an OpenAI-style speech server yourself and set its address under Advanced.";
pub const TTS_NEEDS_TRANSLATION: &str =
    "The voice-over speaks the translation: choose one under Subtitles → Translation";

pub fn tts_installed_status(build: &str, bytes: u64) -> String {
    format!("Installed: {build}, OmniVoice q8_0 ({})", size(bytes))
}

pub fn tts_cli_plan(build: &str, download: u64, dir: &Path) -> String {
    format!(
        "Installing the voice-over: {build} + OmniVoice q8_0, {} to download, into {}",
        size(download),
        dir.display()
    )
}

pub const MENU_VOICE_OVER: &str = "Voice-over (dub the translation)";
pub const MENU_VOICE_OVER_HINT: &str =
    "Speak the translation over the video and turn the original sound down";
pub const SLIDER_ORIGINAL: &str = "Original";
pub const SLIDER_VOICE: &str = "Voice-over";
pub const SECTION_VOICE_OVER: &str = "Voice-over";
pub const TTS_ENABLE: &str = "Speak the translation over the video";
pub const ROW_ORIGINAL_VOLUME: &str = "Original sound, %";
pub const ROW_VOICE_VOLUME: &str = "Voice, %";
pub const TTS_ADVANCED: &str = "Advanced: my own speech server";
pub const ROW_SERVER: &str = "Server address";
pub const HINT_SERVER: &str = "empty = the installed engine; http://host:port/v1";
pub const ROW_TTS_MODEL: &str = "Model";
pub const HINT_TTS_MODEL: &str = "omnivoice";
pub const ROW_VOICE: &str = "Voice";
pub const HINT_VOICE: &str = "the server's default";

pub fn tts_language(lang: &str) -> String {
    format!("Spoken in: {lang} (the \"Translate into\" language)")
}

pub const DUB_STARTING: &str = "Voice-over: starting the voice engine…";
pub const DUB_FIRST_START: &str =
    " the first start on this graphics card takes up to half a minute";
pub const DUB_PREPARING: &str = "Voice-over: preparing…";
pub const DUB_ON: &str = "Voice-over";

pub fn dub_stopped(e: &str) -> String {
    format!("Voice-over stopped: {e}")
}

pub fn dub_line_failed(e: &str) -> String {
    format!("Voice-over: a line could not be spoken ({e})")
}

pub const DUB_ENGINE_GONE: &str = "the voice engine did not start; check the install in Settings";

pub fn dub_engine_quit(times: usize) -> String {
    format!("the voice engine quit unexpectedly {times} times")
}

pub fn dub_cannot_start(e: &dyn std::fmt::Display) -> String {
    format!("could not start the voice engine: {e}")
}

pub const DUB_NO_AUDIO_OUT: &str = "could not open a second audio player for the voice";

// Command line.

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
