//! Helper processes: ffmpeg for extracting subtitles and audio, whisper-server for ASR.

use crate::MediaError;
use gp_core::Ms;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};

fn quiet(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: without it every helper flashes a console window.
        cmd.creation_flags(0x0800_0000);
        win::keep_std_handles_private();
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// Finds a helper binary: explicit path, then next to the executable (in its folder or in a
/// subfolder named after the tool, which is where the Windows package keeps ffmpeg), then PATH.
pub fn locate(tool: &str, configured: &str) -> Result<PathBuf, MediaError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned));
    locate_in(
        tool,
        configured,
        exe_dir.as_deref(),
        &std::env::var_os("PATH").unwrap_or_default(),
    )
}

fn locate_in(
    tool: &str,
    configured: &str,
    exe_dir: Option<&Path>,
    path_var: &std::ffi::OsStr,
) -> Result<PathBuf, MediaError> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(MediaError::ToolNotFound { tool: tool.into() })
        };
    }
    let exe_name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = exe_dir {
        for candidate in [dir.join(&exe_name), dir.join(tool).join(&exe_name)] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    std::env::split_paths(path_var)
        .map(|dir| dir.join(&exe_name))
        .find(|p| p.is_file())
        .ok_or(MediaError::ToolNotFound { tool: tool.into() })
}

fn run(ffmpeg: &Path, args: &[&str]) -> Result<Vec<u8>, MediaError> {
    let mut all = vec!["-nostdin", "-v", "error"];
    all.extend_from_slice(args);
    output_of(ffmpeg, &all)
}

/// Runs a helper without a console window and returns what it printed.
pub fn output_of(program: &Path, args: &[&str]) -> Result<Vec<u8>, MediaError> {
    let mut cmd = quiet(Command::new(program));
    let out = cmd.args(args).output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(MediaError::ToolFailed {
            tool: program
                .file_stem()
                .map_or("tool".into(), |s| s.to_string_lossy().into_owned()),
            stderr: stderr.chars().take(400).collect(),
        })
    }
}

/// Options that go before `-i`: the headers a site wants, the size of the requests.
fn with_input<'a>(input: &'a [String], media: &'a str) -> Vec<&'a str> {
    let mut args: Vec<&str> = input.iter().map(String::as_str).collect();
    // Without a limit a stalled connection leaves ffmpeg, and the transcription behind it,
    // waiting for ever at 0%.
    if media.starts_with("http") {
        args.extend(["-rw_timeout", "20000000"]);
    }
    args.extend(["-i", media]);
    args
}

/// Converts one text subtitle stream (container stream index) to SRT.
pub fn extract_subtitles(
    ffmpeg: &Path,
    media: &str,
    stream_index: u32,
) -> Result<Vec<u8>, MediaError> {
    let map = format!("0:{stream_index}");
    run(ffmpeg, &["-i", media, "-map", &map, "-f", "srt", "-"])
}

/// Decodes `[start, start + duration)` of one audio stream to 16 kHz mono PCM.
/// `stream_index` None picks the default audio stream.
pub fn extract_pcm(
    ffmpeg: &Path,
    media: &str,
    input: &[String],
    stream_index: Option<u32>,
    start: Ms,
    duration: Ms,
) -> Result<Vec<i16>, MediaError> {
    extract_pcm_at(ffmpeg, media, input, stream_index, start, duration, 16_000)
}

/// [`extract_pcm`] at another sample rate.
pub fn extract_pcm_at(
    ffmpeg: &Path,
    media: &str,
    input: &[String],
    stream_index: Option<u32>,
    start: Ms,
    duration: Ms,
    rate: u32,
) -> Result<Vec<i16>, MediaError> {
    let (ss, t, ar) = (secs(start), secs(duration), rate.to_string());
    let map = stream_index.map_or("0:a:0".to_owned(), |i| format!("0:{i}"));
    let mut args = vec!["-ss", &ss, "-t", &t];
    args.extend(with_input(input, media));
    args.extend([
        "-map", &map, "-vn", "-ac", "1", "-ar", &ar, "-f", "s16le", "-",
    ]);
    let bytes = run(ffmpeg, &args)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}

/// Cuts an audio clip for an Anki card.
pub fn extract_clip(
    ffmpeg: &Path,
    media: &str,
    input: &[String],
    stream_index: Option<u32>,
    start: Ms,
    duration: Ms,
    out: &Path,
) -> Result<(), MediaError> {
    let (ss, t) = (secs(start), secs(duration));
    let map = stream_index.map_or("0:a:0".to_owned(), |i| format!("0:{i}"));
    let out = out.to_string_lossy();
    let mut args = vec!["-y", "-ss", &ss, "-t", &t];
    args.extend(with_input(input, media));
    args.extend(["-map", &map, "-vn", "-ac", "1", "-b:a", "64k", &out]);
    run(ffmpeg, &args)?;
    Ok(())
}

fn secs(ms: Ms) -> String {
    format!("{:.3}", ms.max(0) as f64 / 1000.0)
}

/// Every helper server started and not yet dropped, with the name it was started under, so
/// the program can stop them on its way out even when the code that owns them never gets to
/// run its destructors.
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());
type Live = (&'static str, Weak<Mutex<Child>>);
/// The program to run as `<exe> --watchdog <parent pid> <child pid>`; none until set.
static WATCHDOG: OnceLock<PathBuf> = OnceLock::new();

pub const WHISPER: &str = "whisper-server";

/// Starts a watchdog next to every helper server from now on (Windows only): a second copy
/// of `exe`, run with `--watchdog`, that kills the server once this process is gone. The job
/// object does the same, but cannot be relied on when this process already sits in a job
/// of someone else's.
pub fn enable_watchdog(exe: PathBuf) {
    let _ = WATCHDOG.set(exe);
}

/// Kills every helper server still running.
pub fn stop_all() {
    stop_where(|_| true);
}

/// Kills the running servers started under `name`, such as `WHISPER`.
pub fn stop_named(name: &str) {
    stop_where(|n| n == name);
}

fn stop_where(matches: impl Fn(&str) -> bool) {
    let stopping: Vec<Arc<Mutex<Child>>> = {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        let (hit, keep) = std::mem::take(&mut *live)
            .into_iter()
            .partition(|(n, _)| matches(n));
        *live = keep;
        hit.into_iter().filter_map(|(_, w)| w.upgrade()).collect()
    };
    for child in stopping {
        let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A port on 127.0.0.1 that nothing listens on right now.
pub fn free_port() -> Result<u16, MediaError> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// How to start a helper server that answers HTTP on 127.0.0.1.
pub struct ServerCommand<'a> {
    pub name: &'static str,
    pub program: &'a Path,
    pub args: Vec<std::ffi::OsString>,
    pub envs: Vec<(&'static str, std::ffi::OsString)>,
    pub port: u16,
}

/// A running helper server (whisper-server, the voice engine): no console window, killed
/// with this process however it ends, and killed when this is dropped.
pub struct ServerProcess {
    child: Arc<Mutex<Child>>,
    pub url: String,
    pub name: &'static str,
}

pub type WhisperProcess = ServerProcess;

impl ServerProcess {
    pub fn start(c: ServerCommand) -> Result<ServerProcess, MediaError> {
        let mut cmd = quiet(Command::new(c.program));
        cmd.args(&c.args);
        for (k, v) in &c.envs {
            cmd.env(k, v);
        }
        // The CUDA and ggml DLLs sit next to the server binary.
        if let Some(dir) = c.program.parent() {
            cmd.current_dir(dir);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        #[cfg(windows)]
        {
            let _ = win::kill_with_parent(&child);
            if let Some(exe) = WATCHDOG.get() {
                let mut dog = quiet(Command::new(exe));
                dog.arg("--watchdog")
                    .arg(std::process::id().to_string())
                    .arg(child.id().to_string());
                // Not waited for: it ends on its own with the server or after killing it.
                let _ = dog.stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            }
        }
        // Unix has no equivalent here: PR_SET_PDEATHSIG fires when the spawning *thread*
        // exits, and the short-lived ASR thread would take down a server meant to be reused.
        let child = Arc::new(Mutex::new(child));
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|(_, w)| w.strong_count() > 0);
        live.push((c.name, Arc::downgrade(&child)));
        Ok(ServerProcess {
            child,
            url: format!("http://127.0.0.1:{}", c.port),
            name: c.name,
        })
    }

    /// `vad_model` enables the built-in voice activity detector, which stops Whisper from
    /// inventing text over silence and music. `threads` replaces whisper-server's own
    /// default of at most four CPU threads.
    pub fn spawn_with_threads(
        server: &Path,
        model: &Path,
        vad_model: Option<&Path>,
        threads: Option<usize>,
    ) -> Result<ServerProcess, MediaError> {
        let port = free_port()?;
        let mut args: Vec<std::ffi::OsString> = vec!["-m".into(), model.into()];
        for a in [
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "-l",
            "auto",
        ] {
            args.push(a.into());
        }
        if let Some(vad) = vad_model {
            args.extend(["--vad".into(), "-vm".into(), vad.into()]);
        }
        if let Some(t) = threads {
            args.extend(["-t".into(), t.to_string().into()]);
        }
        Self::start(ServerCommand {
            name: WHISPER,
            program: server,
            args,
            envs: Vec::new(),
            port,
        })
    }

    pub fn has_exited(&mut self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        !matches!(child.try_wait(), Ok(None))
    }

    pub fn pid(&self) -> u32 {
        self.child.lock().unwrap_or_else(|e| e.into_inner()).id()
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The `--watchdog <parent pid> <child pid>` mode: waits until either process ends and, if
/// the parent went first, kills the child. Returns the exit code.
pub fn watchdog(parent: &str, child: &str) -> i32 {
    let (Ok(parent), Ok(child)) = (parent.parse::<u32>(), child.parse::<u32>()) else {
        return 2;
    };
    #[cfg(windows)]
    return win::watchdog(parent, child);
    #[cfg(not(windows))]
    {
        let _ = (parent, child);
        2
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::{Once, OnceLock};
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// std spawns children with handle inheritance on, so even with `Stdio::null()` a child
    /// gets a copy of my stdout; a pipe like `glossplay | grep` then stays open for as long
    /// as the child lives. `Stdio::inherit()` duplicates the handle itself, so it still works.
    pub fn keep_std_handles_private() {
        static DONE: Once = Once::new();
        DONE.call_once(|| {
            let handles = [
                io::stdin().as_raw_handle(),
                io::stdout().as_raw_handle(),
                io::stderr().as_raw_handle(),
            ];
            for h in handles.into_iter().filter(|h| !h.is_null()) {
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
            }
        });
    }

    struct Job(HANDLE);
    // The handle is only ever passed to thread-safe kernel calls.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    /// One job for the whole process. I never close its handle: the OS closes it when this
    /// process dies, however it dies, and that kills everything assigned to the job.
    fn job() -> io::Result<HANDLE> {
        static JOB: OnceLock<Result<Job, i32>> = OnceLock::new();
        let job = JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let code = io::Error::last_os_error().raw_os_error().unwrap_or(0);
                CloseHandle(h);
                return Err(code);
            }
            Ok(Job(h))
        });
        match job {
            Ok(j) => Ok(j.0),
            Err(code) => Err(io::Error::from_raw_os_error(*code)),
        }
    }

    pub fn watchdog(parent: u32, child: u32) -> i32 {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{
            INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
            WaitForMultipleObjects,
        };
        unsafe {
            let c = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, child);
            if c.is_null() {
                return 0;
            }
            let p = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent);
            let parent_gone = p.is_null() || {
                let both = [p, c];
                WaitForMultipleObjects(2, both.as_ptr(), 0, INFINITE) == WAIT_OBJECT_0
            };
            if parent_gone {
                TerminateProcess(c, 1);
            }
            if !p.is_null() {
                CloseHandle(p);
            }
            CloseHandle(c);
        }
        0
    }

    pub fn kill_with_parent(child: &Child) -> io::Result<()> {
        let job = job()?;
        if unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gp-locate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn exe_name() -> String {
        format!("ffmpeg{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn locate_goes_from_the_setting_to_the_package_to_path() {
        let root = scratch("order");
        let (app, on_path) = (root.join("app"), root.join("path"));
        let bundled = app.join("ffmpeg").join(exe_name());
        let system = on_path.join(exe_name());
        let custom = root.join("mine").join("my-ffmpeg");
        for p in [&bundled, &system, &custom] {
            touch(p);
        }
        let path_var = std::env::join_paths([&on_path]).unwrap();

        let found = locate_in("ffmpeg", custom.to_str().unwrap(), Some(&app), &path_var);
        assert_eq!(found.unwrap(), custom);
        let found = locate_in("ffmpeg", "", Some(&app), &path_var);
        assert_eq!(found.unwrap(), bundled);
        // Without a suffix the file and the folder would share a name, so this one gets its own app.
        let flat = root.join("flat");
        let beside = flat.join(exe_name());
        touch(&beside);
        let found = locate_in("ffmpeg", "", Some(&flat), &path_var);
        assert_eq!(found.unwrap(), beside);
        let found = locate_in("ffmpeg", "", Some(&root.join("elsewhere")), &path_var);
        assert_eq!(found.unwrap(), system);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn locate_reports_a_wrong_setting_and_a_missing_tool() {
        let root = scratch("wrong");
        touch(&root.join("app").join(exe_name()));
        let missing = root.join("nope").join("ffmpeg");
        let none = std::ffi::OsStr::new("");
        let found = locate_in(
            "ffmpeg",
            missing.to_str().unwrap(),
            Some(&root.join("app")),
            none,
        );
        assert!(matches!(found, Err(MediaError::ToolNotFound { .. })));
        let found = locate_in("ffmpeg", "", Some(&root.join("empty")), none);
        assert!(matches!(found, Err(MediaError::ToolNotFound { .. })));
        let _ = std::fs::remove_dir_all(&root);
    }
}
