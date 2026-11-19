//! The voice-over engine as a downloadable component: the audio.cpp server and the OmniVoice
//! model, and how the server is started.

use crate::components::{self, Component, FileSpec, Unpack};
use crate::setup_text as text;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const ID: &str = "voice";

const RELEASE: &str = "v0.9.5";
const RELEASE_URL: &str = "https://github.com/0xShug0/audio.cpp/releases/download/v0.9.5";
// Pinned to a commit so the file cannot change under the recorded hash.
const MODEL_URL: &str = "https://huggingface.co/audio-cpp/audio.cpp-gguf/resolve/32c1ce1fea57a513eb9386aa62408d47b1cf7ab2/OmniVoice-GGUF/omnivoice-q8_0.gguf";
const MODEL_FILE: &str = "omnivoice-q8_0.gguf";
const MODEL_SIZE: u64 = 1_350_296_096;
const MODEL_SHA256: &str = "0e5a721edd1b08d9067ef66f40ded609a6b14f5a8445c703f9869736cabd350a";
/// The id the model gets in the server's config, and the `model` of every request.
pub const MODEL_ID: &str = "omnivoice";

const SERVER_PATH: &str = "engine/audiocpp_server.exe";
const MODEL_PATH: &str = "models/omnivoice-q8_0.gguf";

/// Ready-made audio.cpp builds are only used on 64-bit Windows here.
pub const ENGINE_AVAILABLE: bool = cfg!(all(windows, target_arch = "x86_64"));

struct Archive {
    file: &'static str,
    size: u64,
    sha256: &'static str,
    files: &'static [&'static str],
    unpacked_size: u64,
}

// The archives also carry the command-line tools, model specs and Python scripts; the
// server needs only itself, the DLLs next to it and nothing else (its model specs are built
// in).
const VULKAN: Archive = Archive {
    file: "audio-v0.9.5-bin-windows-x64-vulkan.zip",
    size: 85_685_046,
    sha256: "dc103660e5fac85860677af2b39361154d863953777ab812d891721e6e163148",
    files: &["audiocpp_server.exe", "*.dll", "LICENSE"],
    unpacked_size: 100_790_650,
};

const CUDA: Archive = Archive {
    file: "audio-v0.9.5-bin-windows-x64-cuda13.3.zip",
    size: 280_869_815,
    sha256: "9d413d8565ad26e1ae33743e9c884e733f1684169be6f8934e1af9ee2e57a872",
    files: &["audiocpp_server.exe", "ggml-cuda.dll", "*.dll", "LICENSE"],
    unpacked_size: 309_679_866,
};

const CUDART: Archive = Archive {
    file: "audio-v0.9.5-cudart-windows-x64-cuda13.3.zip",
    size: 575_457_454,
    sha256: "8614040110cc5852b1dd2192e80344295500f4fa7f3199a51c737c2c7549c3dc",
    files: &[
        "cudart64_13.dll",
        "cublas64_13.dll",
        "cublasLt64_13.dll",
        "*.dll",
    ],
    unpacked_size: 768_815_552,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Vulkan,
    Nvidia,
}

impl Engine {
    pub fn key(self) -> &'static str {
        match self {
            Engine::Vulkan => "vulkan",
            Engine::Nvidia => "nvidia",
        }
    }

    pub fn parse(key: &str) -> Option<Engine> {
        match key {
            "vulkan" => Some(Engine::Vulkan),
            "nvidia" => Some(Engine::Nvidia),
            _ => None,
        }
    }

    /// The `backend` of the server's config.
    pub fn backend(self) -> &'static str {
        match self {
            Engine::Vulkan => "vulkan",
            Engine::Nvidia => "cuda",
        }
    }

    pub fn build_name(self) -> &'static str {
        match self {
            Engine::Vulkan => text::TTS_BUILD_VULKAN,
            Engine::Nvidia => text::TTS_BUILD_NVIDIA,
        }
    }

    pub fn choice_name(self) -> &'static str {
        match self {
            Engine::Vulkan => text::TTS_ENGINE_VULKAN,
            Engine::Nvidia => text::TTS_ENGINE_NVIDIA,
        }
    }
}

/// The install choice from the settings; anything unknown is the Vulkan build, which runs on
/// every graphics card.
pub fn resolve(engine: &str) -> Engine {
    Engine::parse(engine).unwrap_or(Engine::Vulkan)
}

fn archive_spec(a: &Archive, label: &str) -> FileSpec {
    FileSpec {
        url: format!("{RELEASE_URL}/{}", a.file),
        size: a.size,
        sha256: a.sha256.into(),
        unpack: Unpack::Zip {
            from: String::new(),
            files: a.files.iter().map(|f| (*f).to_owned()).collect(),
            into: "engine".into(),
            unpacked_size: a.unpacked_size,
        },
        label: label.into(),
    }
}

pub fn component(engine: Engine) -> Component {
    let mut files = Vec::new();
    if ENGINE_AVAILABLE {
        match engine {
            Engine::Vulkan => files.push(archive_spec(&VULKAN, text::TTS_LABEL_VULKAN)),
            Engine::Nvidia => {
                files.push(archive_spec(&CUDA, text::TTS_LABEL_NVIDIA));
                files.push(archive_spec(&CUDART, text::TTS_LABEL_CUDA_LIBS));
            }
        }
    }
    files.push(FileSpec {
        url: MODEL_URL.into(),
        size: MODEL_SIZE,
        sha256: MODEL_SHA256.into(),
        unpack: Unpack::Plain {
            path: MODEL_PATH.into(),
        },
        label: text::TTS_LABEL_MODEL.into(),
    });
    let engine_key = if ENGINE_AVAILABLE {
        json!(engine.key())
    } else {
        serde_json::Value::Null
    };
    Component {
        id: ID.into(),
        name: text::TTS_COMPONENT_NAME.into(),
        version: format!("{RELEASE}-{}+{MODEL_FILE}", engine.key()),
        files,
        note: [
            text::TTS_LICENCE_NC,
            text::TTS_LICENCE_BOSON,
            text::TTS_NO_IMPERSONATION,
            text::TTS_LICENCE_ENGINE,
        ]
        .join(" "),
        meta: json!({ "engine": engine_key, "release": RELEASE, "warmed_up": false }),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TtsInstall {
    /// `None` where no ready-made engine exists and the user runs their own server.
    pub server: Option<PathBuf>,
    pub model: PathBuf,
    pub engine: Option<Engine>,
    /// The engine has spoken since the install; until then its first start compiles
    /// shaders or kernels for the card and takes a while.
    pub warmed_up: bool,
    pub size: u64,
    pub dir: PathBuf,
}

impl TtsInstall {
    pub fn status(&self) -> String {
        let build = self.engine.map_or(text::ENGINE_OWN, Engine::build_name);
        text::tts_installed_status(build, self.size)
    }
}

pub fn installed() -> Option<TtsInstall> {
    from_state(&components::installed(ID)?)
}

fn from_state(state: &components::Installed) -> Option<TtsInstall> {
    if !state.files.iter().any(|f| f.path == MODEL_PATH) {
        return None;
    }
    let server = state
        .files
        .iter()
        .any(|f| f.path == SERVER_PATH)
        .then(|| state.path(SERVER_PATH));
    let engine = state.meta["engine"].as_str().and_then(Engine::parse);
    Some(TtsInstall {
        engine: engine.filter(|_| server.is_some()),
        server,
        model: state.path(MODEL_PATH),
        warmed_up: state.meta["warmed_up"].as_bool().unwrap_or(true),
        size: state.size(),
        dir: state.dir.clone(),
    })
}

pub fn mark_warmed_up() {
    let _ = components::update_meta(ID, |m| m["warmed_up"] = json!(true));
}

/// Whether Windows has a Vulkan driver at all. Without one the engine falls back to the
/// processor, which is far too slow for a voice-over.
pub fn vulkan_loader_present() -> bool {
    if !cfg!(windows) {
        return true;
    }
    std::env::var_os("SystemRoot")
        .is_some_and(|root| PathBuf::from(root).join("System32/vulkan-1.dll").is_file())
}

/// The server's configuration: one model, kept loaded, on 127.0.0.1 only.
pub fn server_config(engine: Engine, model: &Path, port: u16, threads: usize) -> serde_json::Value {
    json!({
        "host": "127.0.0.1",
        "port": port,
        "backend": engine.backend(),
        "device": 0,
        "threads": threads,
        "lazy_load": false,
        "max_loaded_models": 1,
        "models": [{
            "id": MODEL_ID,
            "family": "omnivoice",
            "path": model.to_string_lossy().replace('\\', "/"),
            "task": "tts",
            "mode": "offline",
        }],
    })
}

/// The server's arguments: its config, a log next to it, and no web interface.
pub fn server_args(config: &Path, log: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--config".into(),
        config.into(),
        "--no-ui".into(),
        "--log".into(),
        "--log-file".into(),
        log.into(),
    ]
}

/// GPU devices the installed server sees, from `--list-devices --json`.
fn listed_devices(server: &Path, engine: Engine) -> Option<Vec<String>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(server);
    cmd.args(["--backend", engine.backend(), "--list-devices", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = server.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().ok()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let start = out.find('{')?;
    let v: serde_json::Value = serde_json::from_str(out[start..].trim()).ok()?;
    Some(
        v["devices"]
            .as_array()?
            .iter()
            .filter(|d| d["type"] == "gpu")
            .filter_map(|d| d["description"].as_str().map(str::to_owned))
            .collect(),
    )
}

/// `--install-tts`: installs with progress lines on stdout; returns the exit code.
pub fn cli_install(engine: &str) -> i32 {
    if !engine.is_empty() && Engine::parse(engine).is_none() {
        eprintln!("{}", text::TTS_CLI_BAD_ENGINE);
        return 1;
    }
    let e = resolve(engine);
    let c = component(e);
    println!(
        "{}",
        text::tts_cli_plan(e.build_name(), c.download_size(), &components::dir(ID))
    );
    for line in [
        text::TTS_LICENCE_NC,
        text::TTS_LICENCE_BOSON,
        text::TTS_NO_IMPERSONATION,
        text::TTS_LICENCE_ENGINE,
    ] {
        println!("licence: {line}");
    }
    if let Some(m) = std::env::var_os(components::ENV_MIRROR).filter(|m| !m.is_empty()) {
        println!("mirror={}", PathBuf::from(m).display());
    }
    let cancel = AtomicBool::new(false);
    let mut last: Option<Instant> = None;
    let result = components::install(&c, &cancel, &mut |p| {
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        last = Some(Instant::now());
        let stage = p.stage.map(|s| text::stage(s, &p.file)).unwrap_or_default();
        println!(
            "progress {} | {}",
            text::percent_line(p.fraction(), p.bytes_per_sec),
            text::file_line(&stage, p.file_done, p.file_total)
        );
    });
    match result.as_ref().ok().and_then(from_state) {
        Some(i) => {
            println!("{}", i.status());
            print_install(&i);
            0
        }
        None => {
            match result {
                Err(e) => eprintln!("{e}"),
                Ok(_) => eprintln!("{}", text::NOT_INSTALLED),
            }
            1
        }
    }
}

fn print_install(i: &TtsInstall) {
    println!("installed_dir={}", i.dir.display());
    if let Some(s) = &i.server {
        println!("server={}", s.display());
    }
    println!("model={}", i.model.display());
    if let Ok(state) = std::fs::read_to_string(i.dir.join("component.json"))
        && let Some(state) = components::parse_state(&state)
    {
        println!("files={}", state.files.len());
        if let Some(m) = state.files.iter().find(|f| f.path == MODEL_PATH) {
            println!("model_sha256={}", m.sha256);
        }
    }
}

/// `--tts-status`: what is installed and what this computer has for it.
pub fn cli_status() -> i32 {
    match installed() {
        Some(i) => {
            println!("installed=yes");
            println!("installed_engine={}", i.engine.map_or("own", Engine::key));
            println!("installed_size={}", text::size(i.size));
            println!("warmed_up={}", i.warmed_up);
            print_install(&i);
            if let (Some(server), Some(engine)) = (&i.server, i.engine) {
                match listed_devices(server, engine) {
                    Some(gpus) if gpus.is_empty() => println!("engine_gpus=none"),
                    Some(gpus) => println!("engine_gpus={}", gpus.join("; ")),
                    None => println!("engine_gpus=unknown"),
                }
            }
        }
        None => println!("installed=no"),
    }
    println!("engine_available={ENGINE_AVAILABLE}");
    println!("vulkan_loader={}", vulkan_loader_present());
    match &crate::asr_setup::hardware_now().gpu {
        Some(g) => println!("nvidia_gpu={} ({} MB)", g.name, g.vram_mb),
        None => println!("nvidia_gpu=none"),
    }
    for e in [Engine::Vulkan, Engine::Nvidia] {
        let c = component(e);
        println!(
            "download_{}={} ({} bytes), disk peak {}",
            e.key(),
            text::size(c.download_size()),
            c.download_size(),
            text::size(c.space_needed())
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_files() {
        let c = component(Engine::Vulkan);
        let names: Vec<&str> = c.files.iter().map(|f| f.file_name()).collect();
        if ENGINE_AVAILABLE {
            assert_eq!(
                names,
                ["audio-v0.9.5-bin-windows-x64-vulkan.zip", MODEL_FILE]
            );
            // About 1.44 GB, as the dialog says.
            assert_eq!(c.download_size(), 1_435_981_142);
            let n = component(Engine::Nvidia);
            assert_eq!(n.files.len(), 3);
            assert_eq!(n.download_size(), 2_206_623_365);
        } else {
            assert_eq!(names, [MODEL_FILE]);
        }
        assert!(c.files.iter().all(|f| f.url.starts_with("https://")));
        assert!(
            c.files
                .iter()
                .all(|f| f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        );
        assert!(c.note.contains("CC BY-NC") && c.note.contains("Boson AI USA, Inc."));
        if ENGINE_AVAILABLE {
            assert_eq!(c.meta["engine"], "vulkan");
        }
    }

    #[test]
    fn unknown_choices_get_the_build_that_runs_everywhere() {
        assert_eq!(resolve("nvidia"), Engine::Nvidia);
        assert_eq!(resolve("auto"), Engine::Vulkan);
        assert_eq!(resolve(""), Engine::Vulkan);
    }

    #[test]
    fn server_config_has_the_model_and_backend() {
        let c = server_config(
            Engine::Nvidia,
            Path::new(r"E:\data\components\voice\models\omnivoice-q8_0.gguf"),
            8123,
            8,
        );
        assert_eq!(c["backend"], "cuda");
        assert_eq!(c["port"], 8123);
        assert_eq!(c["host"], "127.0.0.1");
        assert_eq!(c["models"][0]["id"], MODEL_ID);
        assert_eq!(
            c["models"][0]["path"],
            "E:/data/components/voice/models/omnivoice-q8_0.gguf"
        );
    }

    #[test]
    fn installed_paths_come_from_the_state() {
        let state = |paths: &[&str]| components::Installed {
            id: ID.into(),
            version: "v".into(),
            installed_at: 0,
            files: paths
                .iter()
                .map(|p| components::InstalledFile {
                    path: (*p).into(),
                    size: 2,
                    sha256: String::new(),
                    source: String::new(),
                })
                .collect(),
            meta: json!({ "engine": "vulkan", "warmed_up": false }),
            dir: PathBuf::from("/data/components/voice"),
        };
        let i = from_state(&state(&[SERVER_PATH, MODEL_PATH, "engine/msvcp140.dll"])).unwrap();
        assert_eq!(i.engine, Some(Engine::Vulkan));
        assert!(i.server.unwrap().ends_with("engine/audiocpp_server.exe"));
        assert!(!i.warmed_up);
        assert_eq!(i.size, 6);
        let own = from_state(&state(&[MODEL_PATH])).unwrap();
        assert_eq!((own.server, own.engine), (None, None));
        assert!(from_state(&state(&[SERVER_PATH])).is_none());
    }
}
