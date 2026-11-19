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
