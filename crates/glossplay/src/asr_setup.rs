//! Speech recognition as a downloadable component: the whisper.cpp server, a Whisper model
//! and the Silero voice detector, chosen to suit the graphics card of this computer.

use crate::components::{self, Component, FileSpec, Unpack};
use crate::i18n::{Tr, t};
use crate::setup_text as text;
use serde_json::json;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const ID: &str = "whisper";

/// The whisper.cpp release whose Windows builds I install. Its tag has no binaries of its
/// own on GitHub for newer versions, so this one is pinned.
const RELEASE: &str = "b5454";
const RELEASE_URL: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/b5454";
// Pinned to a commit so the files cannot change under the recorded hashes.
const MODELS_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1";
const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b";
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_SIZE: u64 = 885_098;
const VAD_SHA256: &str = "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf";

/// CUDA 12 runs on any driver from 525 on.
const MIN_DRIVER: u32 = 525;
/// large-v3-turbo takes about 2 GB of video memory; cards sold as 4 GB report a little less.
const TURBO_VRAM_MB: u64 = 3800;
const MAX_CPU_THREADS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Nvidia,
    Cpu,
}

impl Engine {
    pub fn key(self) -> &'static str {
        match self {
            Engine::Nvidia => "nvidia",
            Engine::Cpu => "cpu",
        }
    }

    pub fn parse(key: &str) -> Option<Engine> {
        match key {
            "nvidia" => Some(Engine::Nvidia),
            "cpu" => Some(Engine::Cpu),
            _ => None,
        }
    }

    pub fn build_name(self) -> &'static str {
        match self {
            Engine::Nvidia => t(Tr::BuildNvidia),
            Engine::Cpu => t(Tr::BuildCpu),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Model {
    pub id: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

impl Model {
    pub fn file(&self) -> String {
        format!("ggml-{}.bin", self.id)
    }
}

pub const MODELS: [Model; 4] = [
    Model {
        id: "large-v3-turbo",
        size: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
    Model {
        id: "large-v3-turbo-q5_0",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    Model {
        id: "small",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    Model {
        id: "base",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

struct Build {
    file: &'static str,
    size: u64,
    sha256: &'static str,
    files: &'static [&'static str],
    unpacked_size: u64,
}

const CUDA_BUILD: Build = Build {
    file: "whisper-bin-win-cuda-12.4.0-x64.zip",
    size: 684_913_404,
    sha256: "afef0b881c500958921c3f5523b50e59ee2ec9b6f5cbd25b324c51ed308a957a",
    // The archive also carries llama, parakeet, SDL and two dozen tools the server does not
    // load; only these are needed (checked against the DLL imports).
    files: &[
        "whisper-server.exe",
        "whisper.dll",
        "ggml.dll",
        "ggml-base.dll",
        "ggml-cpu-*.dll",
        "ggml-cuda.dll",
        "cudart64_12.dll",
        "cublas64_12.dll",
        "cublasLt64_12.dll",
    ],
    unpacked_size: 1_155_773_952,
};

const CPU_BUILD: Build = Build {
    file: "whisper-bin-x64.zip",
    size: 8_928_640,
    sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
    files: &[
        "whisper-server.exe",
        "whisper.dll",
        "ggml.dll",
        "ggml-base.dll",
        "ggml-cpu-*.dll",
    ],
    unpacked_size: 11_850_240,
};

/// Ready-made whisper-server builds exist for 64-bit Windows only.
pub const ENGINE_AVAILABLE: bool = cfg!(all(windows, target_arch = "x86_64"));

const SERVER_PATH: &str = "engine/whisper-server.exe";

#[derive(Debug, Clone, PartialEq)]
pub struct Gpu {
    pub name: String,
    pub vram_mb: u64,
    pub driver: String,
}

impl Gpu {
    fn driver_major(&self) -> Option<u32> {
        self.driver.split('.').next()?.trim().parse().ok()
    }

    /// Unknown driver versions get the benefit of the doubt.
    fn driver_ok(&self) -> bool {
        self.driver_major().is_none_or(|v| v >= MIN_DRIVER)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hardware {
    pub gpu: Option<Gpu>,
    pub cores: usize,
}

/// The NVIDIA card with the most memory, from
/// `nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi(out: &str) -> Option<Gpu> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.rsplitn(3, ',');
            let driver = fields.next()?.trim();
            let vram_mb = fields.next()?.trim().parse().ok()?;
            let name = fields.next()?.trim();
            (!name.is_empty()).then(|| Gpu {
                name: name.to_owned(),
                vram_mb,
                driver: driver.to_owned(),
            })
        })
        .max_by_key(|g| g.vram_mb)
}

fn nvidia_smi() -> Option<PathBuf> {
    let name = format!("nvidia-smi{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot") {
            candidates.push(PathBuf::from(root).join("System32").join(&name));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            candidates.push(
                PathBuf::from(pf)
                    .join("NVIDIA Corporation")
                    .join("NVSMI")
                    .join(&name),
            );
        }
    }
    candidates.extend(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join(&name)),
    );
    candidates.into_iter().find(|p| p.is_file())
}

/// Asks the NVIDIA driver which card is there. `None` without NVIDIA hardware or driver.
pub fn detect_gpu() -> Option<Gpu> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(nvidia_smi()?);
    cmd.args([
        "--query-gpu=name,memory.total,driver_version",
        "--format=csv,noheader,nounits",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW, or a console flashes up.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    // A wedged driver can make nvidia-smi hang; ten seconds is far more than it needs.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_nvidia_smi(&out)
}

fn detect() -> Hardware {
    Hardware {
        gpu: detect_gpu(),
        cores: physical_cores(),
    }
}

static HARDWARE: OnceLock<Hardware> = OnceLock::new();

/// What this computer has, or `None` while it is still being found out; the first call
/// starts the check in the background, since nvidia-smi takes a moment.
pub fn hardware() -> Option<&'static Hardware> {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            let _ = HARDWARE.set(detect());
        });
    });
    HARDWARE.get()
}

pub fn hardware_now() -> &'static Hardware {
    HARDWARE.get_or_init(detect)
}

pub fn physical_cores() -> usize {
    #[cfg(windows)]
    if let Some(n) = windows_physical_cores() {
        return n;
    }
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

#[cfg(windows)]
fn windows_physical_cores() -> Option<usize> {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformation, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION,
    };
    let item = size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
    let mut len = 0u32;
    unsafe { GetLogicalProcessorInformation(std::ptr::null_mut(), &mut len) };
    let count = len as usize / item;
    if count == 0 {
        return None;
    }
    let mut buf: Vec<SYSTEM_LOGICAL_PROCESSOR_INFORMATION> =
        vec![unsafe { std::mem::zeroed() }; count];
    if unsafe { GetLogicalProcessorInformation(buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let cores = buf[..(len as usize / item).min(count)]
        .iter()
        .filter(|i| i.Relationship == RelationProcessorCore)
        .count();
    (cores > 0).then_some(cores)
}

/// Threads for the processor build: one per physical core, since Whisper gains nothing from
/// hyper-threads, and no more than eight, past which it stops getting faster.
pub fn cpu_threads(physical_cores: usize) -> usize {
    physical_cores.clamp(1, MAX_CPU_THREADS)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variant {
    pub engine: Engine,
    pub model: &'static Model,
}

fn default_model(engine: Engine, gpu: Option<&Gpu>) -> &'static Model {
    let id = match engine {
        Engine::Nvidia if gpu.is_some_and(|g| g.vram_mb < TURBO_VRAM_MB) => "large-v3-turbo-q5_0",
        Engine::Nvidia => "large-v3-turbo",
        Engine::Cpu => "small",
    };
    model(id).expect("default models are in the table")
}

/// The variant for this hardware when the user has not chosen one.
pub fn recommend(gpu: Option<&Gpu>) -> Variant {
    let engine = match gpu {
        Some(g) if g.driver_ok() => Engine::Nvidia,
        _ => Engine::Cpu,
    };
    Variant {
        engine,
        model: default_model(engine, gpu),
    }
}

/// The variant to install from the settings: `engine` is "auto", "nvidia" or "cpu", `model`
/// a model id or empty for the one that suits the engine.
pub fn resolve(engine: &str, model_id: &str, gpu: Option<&Gpu>) -> Variant {
    let engine = Engine::parse(engine).unwrap_or_else(|| recommend(gpu).engine);
    Variant {
        engine,
        model: model(model_id).unwrap_or_else(|| default_model(engine, gpu)),
    }
}

/// Why the automatic choice is not the NVIDIA build although there is an NVIDIA card.
pub fn old_driver(gpu: Option<&Gpu>) -> bool {
    gpu.is_some_and(|g| !g.driver_ok())
}

pub fn component(v: &Variant) -> Component {
    let mut files = Vec::new();
    if ENGINE_AVAILABLE {
        let (build, label) = match v.engine {
            Engine::Nvidia => (&CUDA_BUILD, t(Tr::EngineLabelNvidia)),
            Engine::Cpu => (&CPU_BUILD, t(Tr::EngineLabelCpu)),
        };
        files.push(FileSpec {
            url: format!("{RELEASE_URL}/{}", build.file),
            size: build.size,
            sha256: build.sha256.into(),
            unpack: Unpack::Zip {
                from: "Release".into(),
                files: build.files.iter().map(|f| (*f).to_owned()).collect(),
                into: "engine".into(),
                unpacked_size: build.unpacked_size,
            },
            label: label.into(),
        });
    }
    files.push(FileSpec {
        url: format!("{MODELS_URL}/{}", v.model.file()),
        size: v.model.size,
        sha256: v.model.sha256.into(),
        unpack: Unpack::Plain {
            path: format!("models/{}", v.model.file()),
        },
        label: text::model_label(v.model.id),
    });
    // The player looks for the voice detector next to the model.
    files.push(FileSpec {
        url: format!("{VAD_URL}/{VAD_FILE}"),
        size: VAD_SIZE,
        sha256: VAD_SHA256.into(),
        unpack: Unpack::Plain {
            path: format!("models/{VAD_FILE}"),
        },
        label: t(Tr::VadLabel).into(),
    });
    let engine = if ENGINE_AVAILABLE {
        json!(v.engine.key())
    } else {
        serde_json::Value::Null
    };
    Component {
        id: ID.into(),
        name: t(Tr::ComponentName).into(),
        version: format!("{RELEASE}-{}+{}", v.engine.key(), v.model.id),
        files,
        note: t(Tr::LicenceNote).into(),
        meta: json!({
            "engine": engine,
            "model": v.model.id,
            "release": RELEASE,
            "warmed_up": false,
        }),
    }
}

/// The speech recognizer as installed by the button.
#[derive(Debug, Clone, PartialEq)]
pub struct AsrInstall {
    /// `None` where no ready-made build exists and the user brings their own.
    pub server: Option<PathBuf>,
    pub model: PathBuf,
    pub engine: Option<Engine>,
    pub model_id: String,
    /// The server has transcribed something since the install; until then the NVIDIA build
    /// may spend a minute compiling for the card.
    pub warmed_up: bool,
    pub size: u64,
    pub dir: PathBuf,
}

impl AsrInstall {
    pub fn status(&self) -> String {
        match self.engine {
            Some(e) => text::installed_status(e.build_name(), &self.model_id, self.size),
            None => text::installed_models_only(&self.model_id),
        }
    }
}

pub fn installed() -> Option<AsrInstall> {
    from_state(&components::installed(ID)?)
}

fn from_state(state: &components::Installed) -> Option<AsrInstall> {
    let model_id = state.meta["model"].as_str()?.to_owned();
    let model_path = format!("models/ggml-{model_id}.bin");
    if !state.files.iter().any(|f| f.path == model_path) {
        return None;
    }
    let engine = state.meta["engine"].as_str().and_then(Engine::parse);
    let server = state
        .files
        .iter()
        .any(|f| f.path == SERVER_PATH)
        .then(|| state.path(SERVER_PATH));
    Some(AsrInstall {
        engine: engine.filter(|_| server.is_some()),
        server,
        model: state.path(&model_path),
        model_id,
        warmed_up: state.meta["warmed_up"].as_bool().unwrap_or(true),
        size: state.size(),
        dir: state.dir.clone(),
    })
}

pub fn mark_warmed_up() {
    let _ = components::update_meta(ID, |m| m["warmed_up"] = json!(true));
}

/// Server and model to run: paths set in the settings win, then the installed component,
/// then a whisper-server next to the program or on PATH.
pub fn resolve_paths(
    cfg: &gp_core::config::Asr,
    installed: Option<&AsrInstall>,
) -> Result<(PathBuf, PathBuf), String> {
    let own_server = !cfg.server_path.is_empty();
    let server = match installed.and_then(|i| i.server.clone()) {
        Some(s) if !own_server => s,
        _ => gp_media::tools::locate("whisper-server", &cfg.server_path).map_err(|_| {
            if own_server || installed.is_some() {
                t(Tr::NoServer).to_owned()
            } else {
                t(Tr::NotSetUp).to_owned()
            }
        })?,
    };
    let model = match installed {
        Some(i) if cfg.model_path.is_empty() => i.model.clone(),
        _ => PathBuf::from(&cfg.model_path),
    };
    if !model.is_file() {
        return Err(if cfg.model_path.is_empty() {
            t(Tr::NotSetUp).to_owned()
        } else {
            t(Tr::NoModel).to_owned()
        });
    }
    Ok((server, model))
}

/// Whether the Visual C++ runtime the Windows builds link against is missing.
pub fn vc_runtime_missing() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let Some(root) = std::env::var_os("SystemRoot") else {
        return false;
    };
    let system = PathBuf::from(root).join("System32");
    [
        "msvcp140.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "vcomp140.dll",
    ]
    .iter()
    .any(|dll| !system.join(dll).is_file())
}

/// `--install-asr`: installs with progress lines on stdout; returns the exit code.
pub fn cli_install(engine: &str, model_id: &str) -> i32 {
    if !matches!(engine, "auto" | "nvidia" | "cpu") {
        eprintln!("{}", text::CLI_BAD_ENGINE);
        return 1;
    }
    if !model_id.is_empty() && model_id != "auto" && model(model_id).is_none() {
        let known: Vec<&str> = MODELS.iter().map(|m| m.id).collect();
        eprintln!("{}", text::cli_bad_model(&known));
        return 1;
    }
    let hw = hardware_now();
    let v = resolve(engine, model_id, hw.gpu.as_ref());
    let c = component(&v);
    println!(
        "{}",
        text::cli_plan(
            if ENGINE_AVAILABLE {
                v.engine.build_name()
            } else {
                t(Tr::EngineOwn)
            },
            v.model.id,
            c.download_size(),
            &components::dir(ID)
        )
    );
    if !ENGINE_AVAILABLE {
        println!("{}", t(Tr::NoEngineHere));
    }
    if let Some(m) = std::env::var_os(components::ENV_MIRROR).filter(|m| !m.is_empty()) {
        println!("mirror={}", PathBuf::from(m).display());
    }
    let cancel = AtomicBool::new(false);
    let mut last: Option<Instant> = None;
    let mut announced = String::new();
    let result = components::install(&c, &cancel, &mut |p| {
        if p.stage == Some(components::Stage::Downloading)
            && p.resumed_from > 0
            && announced != p.file
        {
            announced.clone_from(&p.file);
            println!("{}", text::resuming(&p.file, p.resumed_from, p.file_total));
        }
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
            println!("installed_dir={}", i.dir.display());
            if let Some(s) = &i.server {
                println!("server={}", s.display());
            }
            println!("model={}", i.model.display());
            0
        }
        None => {
            match result {
                Err(e) => eprintln!("{e}"),
                Ok(_) => eprintln!("{}", t(Tr::NotInstalled)),
            }
            1
        }
    }
}

/// `--asr-status`: what is installed and what this computer would get.
pub fn cli_status() -> i32 {
    match installed() {
        Some(i) => {
            println!("installed=yes");
            println!("installed_engine={}", i.engine.map_or("own", Engine::key));
            println!("installed_model={}", i.model_id);
            println!("installed_size={}", text::size(i.size));
            println!("installed_dir={}", i.dir.display());
            if let Some(s) = &i.server {
                println!("server={}", s.display());
            }
            println!("model={}", i.model.display());
            println!("warmed_up={}", i.warmed_up);
        }
        None => println!("installed=no"),
    }
    let hw = hardware_now();
    match &hw.gpu {
        Some(g) => {
            println!("gpu={}", g.name);
            println!("gpu_vram_mb={}", g.vram_mb);
            println!("gpu_driver={}", g.driver);
        }
        None => println!("gpu=none"),
    }
    println!("cpu_cores={}", hw.cores);
    let v = recommend(hw.gpu.as_ref());
    println!("engine_available={ENGINE_AVAILABLE}");
    println!("recommended_engine={}", v.engine.key());
    println!("recommended_model={}", v.model.id);
    let c = component(&v);
    println!("recommended_download={}", text::size(c.download_size()));
    println!("recommended_disk_peak={}", text::size(c.space_needed()));
    if v.engine == Engine::Cpu {
        println!("cpu_threads={}", cpu_threads(hw.cores));
    }
    println!(
        "vc_runtime={}",
        if vc_runtime_missing() {
            "missing"
        } else {
            "ok"
        }
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(vram_mb: u64, driver: &str) -> Gpu {
        Gpu {
            name: "NVIDIA GeForce RTX 5070 Ti".into(),
            vram_mb,
            driver: driver.into(),
        }
    }

    fn pick(v: Variant) -> (Engine, &'static str) {
        (v.engine, v.model.id)
    }

    #[test]
    fn variant_follows_the_graphics_card() {
        let big = gpu(16_303, "617.42");
        assert_eq!(
            pick(recommend(Some(&big))),
            (Engine::Nvidia, "large-v3-turbo")
        );
        assert_eq!(
            pick(recommend(Some(&gpu(4_096, "560.1")))),
            (Engine::Nvidia, "large-v3-turbo")
        );
        assert_eq!(
            pick(recommend(Some(&gpu(2_048, "560.1")))),
            (Engine::Nvidia, "large-v3-turbo-q5_0")
        );
        assert_eq!(pick(recommend(None)), (Engine::Cpu, "small"));
        assert_eq!(
            pick(recommend(Some(&gpu(8_192, "470.82")))),
            (Engine::Cpu, "small"),
            "CUDA 12 does not run on this driver"
        );
        assert!(old_driver(Some(&gpu(8_192, "470.82"))));
        assert!(!old_driver(Some(&big)));
        assert_eq!(
            pick(recommend(Some(&gpu(8_192, "weird")))),
            (Engine::Nvidia, "large-v3-turbo")
        );
    }

    #[test]
    fn manual_choices_override_the_recommendation() {
        let big = gpu(16_303, "617.42");
        assert_eq!(pick(resolve("cpu", "", Some(&big))), (Engine::Cpu, "small"));
        assert_eq!(pick(resolve("auto", "base", None)), (Engine::Cpu, "base"));
        assert_eq!(
            pick(resolve("nvidia", "", None)),
            (Engine::Nvidia, "large-v3-turbo")
        );
        assert_eq!(
            pick(resolve("auto", "no-such-model", Some(&big))),
            (Engine::Nvidia, "large-v3-turbo")
        );
        assert_eq!(
            pick(resolve("", "small", Some(&big))),
            (Engine::Nvidia, "small")
        );
    }

    #[test]
    fn nvidia_smi_output() {
        let one = "NVIDIA GeForce RTX 5070 Ti, 16303, 617.42\r\n";
        assert_eq!(parse_nvidia_smi(one), Some(gpu(16_303, "617.42")));
        let two = "Quadro P400, 2048, 560.1\nNVIDIA GeForce RTX 3060, 12288, 560.1\n";
        assert_eq!(
            parse_nvidia_smi(two).unwrap().name,
            "NVIDIA GeForce RTX 3060"
        );
        let comma = "Card, with comma, 8192, 550.0";
        assert_eq!(parse_nvidia_smi(comma).unwrap().name, "Card, with comma");
        assert_eq!(parse_nvidia_smi(""), None);
        assert_eq!(
            parse_nvidia_smi("NVIDIA-SMI has failed because it couldn't communicate"),
            None
        );
        assert_eq!(parse_nvidia_smi("Card, [N/A], 550.0"), None);
    }

    #[test]
    fn processor_threads() {
        assert_eq!(cpu_threads(0), 1);
        assert_eq!(cpu_threads(4), 4);
        assert_eq!(cpu_threads(8), 8);
        assert_eq!(cpu_threads(14), 8);
    }

    #[test]
    fn component_files() {
        let v = Variant {
            engine: Engine::Nvidia,
            model: model("large-v3-turbo").unwrap(),
        };
        let c = component(&v);
        let names: Vec<&str> = c.files.iter().map(|f| f.file_name()).collect();
        if ENGINE_AVAILABLE {
            assert_eq!(
                names,
                [
                    "whisper-bin-win-cuda-12.4.0-x64.zip",
                    "ggml-large-v3-turbo.bin",
                    "ggml-silero-v5.1.2.bin"
                ]
            );
            // About 2.3 GB, as the dialog promises.
            assert_eq!(c.download_size(), 2_310_353_777);
        } else {
            assert_eq!(names, ["ggml-large-v3-turbo.bin", "ggml-silero-v5.1.2.bin"]);
        }
        assert!(c.files.iter().all(|f| f.url.starts_with("https://")));
        assert!(
            c.files
                .iter()
                .all(|f| f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        );
        let cpu = component(&resolve("cpu", "base", None));
        assert_eq!(cpu.version, "b5454-cpu+base");
        assert_eq!(cpu.meta["model"], "base");
    }

    fn state(meta: serde_json::Value, paths: &[&str]) -> components::Installed {
        components::Installed {
            id: ID.into(),
            version: "v".into(),
            installed_at: 0,
            files: paths
                .iter()
                .map(|p| components::InstalledFile {
                    path: (*p).into(),
                    size: 1,
                    sha256: String::new(),
                    source: String::new(),
                })
                .collect(),
            meta,
            dir: PathBuf::from("/data/components/whisper"),
        }
    }

    #[test]
    fn installed_paths_come_from_the_state() {
        let s = state(
            json!({ "engine": "cpu", "model": "base", "warmed_up": false }),
            &[
                "engine/whisper-server.exe",
                "models/ggml-base.bin",
                "models/ggml-silero-v5.1.2.bin",
            ],
        );
        let i = from_state(&s).unwrap();
        assert_eq!(i.engine, Some(Engine::Cpu));
        assert_eq!(
            i.server.as_deref(),
            Some(PathBuf::from("/data/components/whisper/engine/whisper-server.exe").as_path())
        );
        assert!(i.model.ends_with("models/ggml-base.bin"));
        assert!(!i.warmed_up);

        let models_only = state(
            json!({ "engine": null, "model": "small" }),
            &["models/ggml-small.bin"],
        );
        let i = from_state(&models_only).unwrap();
        assert_eq!((i.server, i.engine), (None, None));
        assert!(i.warmed_up);

        let wrong = state(json!({ "model": "small" }), &["models/ggml-base.bin"]);
        assert!(from_state(&wrong).is_none());
    }

    #[test]
    fn paths_set_by_hand_win() {
        let dir = std::env::temp_dir().join(format!("gp-asr-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (server, model, mine) = (
            dir.join("whisper-server.exe"),
            dir.join("ggml-base.bin"),
            dir.join("mine.bin"),
        );
        for f in [&server, &model, &mine] {
            std::fs::write(f, b"x").unwrap();
        }
        let inst = AsrInstall {
            server: Some(server.clone()),
            model: model.clone(),
            engine: Some(Engine::Cpu),
            model_id: "base".into(),
            warmed_up: true,
            size: 2,
            dir: dir.clone(),
        };
        let mut cfg = gp_core::config::Asr::default();
        assert_eq!(
            resolve_paths(&cfg, Some(&inst)),
            Ok((server.clone(), model.clone()))
        );
        cfg.model_path = mine.to_string_lossy().into_owned();
        assert_eq!(resolve_paths(&cfg, Some(&inst)), Ok((server, mine)));
        cfg.server_path = dir.join("missing.exe").to_string_lossy().into_owned();
        assert_eq!(
            resolve_paths(&cfg, Some(&inst)),
            Err(t(Tr::NoServer).to_owned())
        );
    }
}
