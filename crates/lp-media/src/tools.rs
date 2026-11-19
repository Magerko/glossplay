//! Helper processes: ffmpeg for extracting subtitles and audio, whisper-server for ASR.

use crate::MediaError;
use lp_core::Ms;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

/// Finds a helper binary: explicit path, then next to the executable, then PATH.
pub fn locate(tool: &str, configured: &str) -> Result<PathBuf, MediaError> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(MediaError::ToolNotFound { tool: tool.into() })
        };
    }
    let exe_name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned))
    {
        for candidate in [dir.join(&exe_name), dir.join(tool).join(&exe_name)] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(&exe_name))
        .find(|p| p.is_file())
        .ok_or(MediaError::ToolNotFound { tool: tool.into() })
}

fn run(ffmpeg: &Path, args: &[&str]) -> Result<Vec<u8>, MediaError> {
    let mut cmd = quiet(Command::new(ffmpeg));
    cmd.args(["-nostdin", "-v", "error"]).args(args);
    let out = cmd.output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(MediaError::ToolFailed {
            tool: "ffmpeg".into(),
            stderr: stderr.chars().take(400).collect(),
        })
    }
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
    stream_index: Option<u32>,
    start: Ms,
    duration: Ms,
) -> Result<Vec<i16>, MediaError> {
    let (ss, t) = (secs(start), secs(duration));
    let map = stream_index.map_or("0:a:0".to_owned(), |i| format!("0:{i}"));
    let bytes = run(
        ffmpeg,
        &[
            "-ss", &ss, "-t", &t, "-i", media, "-map", &map, "-vn", "-ac", "1", "-ar", "16000",
            "-f", "s16le", "-",
        ],
    )?;
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
    stream_index: Option<u32>,
    start: Ms,
    duration: Ms,
    out: &Path,
) -> Result<(), MediaError> {
    let (ss, t) = (secs(start), secs(duration));
    let map = stream_index.map_or("0:a:0".to_owned(), |i| format!("0:{i}"));
    let out = out.to_string_lossy();
    run(
        ffmpeg,
        &[
            "-y", "-ss", &ss, "-t", &t, "-i", media, "-map", &map, "-vn", "-ac", "1", "-b:a",
            "64k", &out,
        ],
    )?;
    Ok(())
}

fn secs(ms: Ms) -> String {
    format!("{:.3}", ms.max(0) as f64 / 1000.0)
}

/// A running whisper-server; the process is killed when this is dropped.
pub struct WhisperProcess {
    child: Child,
    pub url: String,
}

impl WhisperProcess {
    /// `vad_model` enables the built-in voice activity detector, which stops Whisper from
    /// inventing text over silence and music.
    pub fn spawn(
        server: &Path,
        model: &Path,
        vad_model: Option<&Path>,
    ) -> Result<WhisperProcess, MediaError> {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let mut cmd = quiet(Command::new(server));
        cmd.arg("-m").arg(model).args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "-l",
            "auto",
        ]);
        if let Some(vad) = vad_model {
            cmd.arg("--vad").arg("-vm").arg(vad);
        }
        // The CUDA and ggml DLLs sit next to the server binary.
        if let Some(dir) = server.parent() {
            cmd.current_dir(dir);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        // A failed assignment only loses the crash safety net; a normal exit still kills it.
        #[cfg(windows)]
        let _ = win::kill_with_parent(&child);
        // Unix has no equivalent here: PR_SET_PDEATHSIG fires when the spawning *thread*
        // exits, and the short-lived ASR thread would take down a server meant to be reused.
        Ok(WhisperProcess {
            child,
            url: format!("http://127.0.0.1:{port}"),
        })
    }

    pub fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for WhisperProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    /// gets a copy of my stdout; a pipe like `linguaplay | grep` then stays open for as long
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

    pub fn kill_with_parent(child: &Child) -> io::Result<()> {
        let job = job()?;
        if unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
