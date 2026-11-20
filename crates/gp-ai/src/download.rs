//! Streams a large file straight to disk. Model files run to gigabytes, so nothing is held
//! in memory, an interrupted download continues where it stopped, and the caller can cancel.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_REDIRECTS: u32 = 5;
const CHUNK: usize = 256 * 1024;

#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    /// The URL is not https, or a redirect led away from https.
    Insecure(String),
    Http(u16),
    Network(String),
    /// The server answered something other than the file asked for.
    BadResponse(String),
    Disk(std::io::Error),
}

impl DownloadError {
    /// Trying again a little later may well succeed, and keeps what is on disk.
    pub fn is_transient(&self) -> bool {
        match self {
            DownloadError::Network(_) => true,
            DownloadError::Http(status) => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Cancelled => write!(f, "cancelled"),
            DownloadError::Insecure(e) => write!(f, "refusing an insecure download: {e}"),
            DownloadError::Http(status) => write!(f, "the server answered HTTP {status}"),
            DownloadError::Network(e) => write!(f, "network error: {e}"),
            DownloadError::BadResponse(e) => write!(f, "unexpected answer from the server: {e}"),
            DownloadError::Disk(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                write!(f, "the disk is full")
            }
            DownloadError::Disk(e) => write!(f, "could not write the file: {e}"),
        }
    }
}

impl std::error::Error for DownloadError {}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            // No global timeout: a 1.6 GB model on a slow line takes as long as it takes. A
            // stalled connection is caught by the per-read limit instead.
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .max_redirects(MAX_REDIRECTS)
            .build()
            .into()
    })
}

/// Downloads `url` into `part`, continuing from what `part` already holds when the server
/// supports ranges. `expected_size`, when known, guards against a server sending more.
/// `progress` gets the number of bytes in `part` so far. On cancel or error `part` stays
/// on disk for the next attempt; checking its hash is the caller's job.
pub fn download_to_file(
    url: &str,
    part: &Path,
    expected_size: Option<u64>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, DownloadError> {
    if !url.starts_with("https://") {
        return Err(DownloadError::Insecure(url.to_owned()));
    }
    download(url, part, expected_size, cancel, progress, false)
}

/// Where a ranged reply starts, from `Content-Range: bytes 100-199/200`.
fn range_start(content_range: &str) -> Option<u64> {
    let rest = content_range.trim().strip_prefix("bytes")?.trim_start();
    rest.split('-').next()?.trim().parse().ok()
}

fn download(
    url: &str,
    part: &Path,
    expected_size: Option<u64>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
    allow_http: bool,
) -> Result<u64, DownloadError> {
    let mut have = std::fs::metadata(part).map_or(0, |m| m.len());
    if expected_size.is_some_and(|size| have > size) {
        have = 0;
    }
    if have > 0 && expected_size == Some(have) {
        progress(have);
        return Ok(have);
    }
    // A second pass is needed only when the server rejects or misreads the range.
    for _ in 0..2 {
        let mut req = agent()
            .get(url)
            // Ranges count bytes of the encoded body; a compressed reply would break resuming.
            .header("Accept-Encoding", "identity");
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut resp = req
            .config()
            .https_only(!allow_http)
            .build()
            .call()
            .map_err(|e| match e {
                ureq::Error::RequireHttpsOnly(e) => DownloadError::Insecure(e),
                other => DownloadError::Network(other.to_string()),
            })?;
        let status = resp.status().as_u16();
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let append = match status {
            206 if have > 0 => {
                if header("content-range").as_deref().and_then(range_start) != Some(have) {
                    have = 0;
                    continue;
                }
                true
            }
            200 => {
                have = 0;
                false
            }
            // The part file holds at least the whole file already, or the server lost track
            // of it. Without a known size I let the hash check decide.
            416 if have > 0 && expected_size.is_none() => return Ok(have),
            416 if have > 0 => {
                have = 0;
                continue;
            }
            _ => return Err(DownloadError::Http(status)),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(part)
            .map_err(DownloadError::Disk)?;
        let mut reader = resp.body_mut().as_reader();
        let mut buf = vec![0u8; CHUNK];
        let mut done = have;
        progress(done);
        loop {
            if cancel.load(Ordering::Relaxed) {
                file.flush().map_err(DownloadError::Disk)?;
                return Err(DownloadError::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = file.flush();
                    return Err(DownloadError::Network(e.to_string()));
                }
            };
            file.write_all(&buf[..n]).map_err(DownloadError::Disk)?;
            done += n as u64;
            if expected_size.is_some_and(|size| done > size) {
                drop(file);
                let _ = std::fs::remove_file(part);
                return Err(DownloadError::BadResponse(
                    "the file is larger than expected".into(),
                ));
            }
            progress(done);
        }
        file.sync_all().map_err(DownloadError::Disk)?;
        return Ok(done);
    }
    Err(DownloadError::BadResponse(
        "the server does not resume downloads correctly".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    fn body() -> Vec<u8> {
        (0..100_000u32).map(|i| (i % 251) as u8).collect()
    }

    /// Where a test downloads to; the directory goes when the first value is dropped.
    fn temp(name: &str) -> (gp_core::scratch::TempDir, std::path::PathBuf) {
        let dir = gp_core::scratch::TempDir::new(&format!("gp-dl-{name}"));
        let part = dir.join("file.part");
        (dir, part)
    }

    #[test]
    fn content_range_start() {
        assert_eq!(range_start("bytes 100-199/200"), Some(100));
        assert_eq!(range_start("bytes 0-0/1"), Some(0));
        assert_eq!(range_start("bytes */200"), None);
        assert_eq!(range_start("items 1-2/3"), None);
    }

    #[test]
    fn plain_http_is_refused() {
        let (_dir, part) = temp("http");
        let no = AtomicBool::new(false);
        let e = download_to_file("http://example.com/x", &part, None, &no, &mut |_| {});
        assert!(matches!(e, Err(DownloadError::Insecure(_))), "{e:?}");
    }

    #[test]
    fn streams_the_whole_file() {
        let data = body();
        let srv = TestServer::start_ranged(data.clone(), true, None);
        let (_dir, part) = temp("whole");
        let no = AtomicBool::new(false);
        let mut seen = Vec::new();
        let n = download(
            &srv.url,
            &part,
            Some(data.len() as u64),
            &no,
            &mut |d| seen.push(d),
            true,
        )
        .unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(std::fs::read(&part).unwrap(), data);
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(seen.last(), Some(&(data.len() as u64)));
    }

    #[test]
    fn resumes_after_a_broken_connection() {
        let data = body();
        let srv = TestServer::start_ranged(data.clone(), true, Some(30_000));
        let (_dir, part) = temp("resume");
        let no = AtomicBool::new(false);
        let size = Some(data.len() as u64);
        let first = download(&srv.url, &part, size, &no, &mut |_| {}, true);
        assert!(matches!(first, Err(DownloadError::Network(_))), "{first:?}");
        let kept = std::fs::metadata(&part).unwrap().len();
        assert!(kept > 0 && kept < data.len() as u64, "{kept}");

        let mut first_report = None;
        let n = download(
            &srv.url,
            &part,
            size,
            &no,
            &mut |d| {
                first_report.get_or_insert(d);
            },
            true,
        )
        .unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(std::fs::read(&part).unwrap(), data);
        assert_eq!(first_report, Some(kept), "progress continues from the part");
        let log = srv.received.lock().unwrap();
        assert!(
            log[1]
                .headers
                .to_ascii_lowercase()
                .contains(&format!("range: bytes={kept}-"))
        );
    }

    #[test]
    fn starts_over_when_the_server_ignores_ranges() {
        let data = body();
        let srv = TestServer::start_ranged(data.clone(), false, None);
        let (_dir, part) = temp("norange");
        std::fs::write(&part, b"stale bytes from somewhere else").unwrap();
        let no = AtomicBool::new(false);
        download(
            &srv.url,
            &part,
            Some(data.len() as u64),
            &no,
            &mut |_| {},
            true,
        )
        .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), data);
    }

    #[test]
    fn cancel_keeps_the_part() {
        let data = body();
        let srv = TestServer::start_ranged(data.clone(), true, None);
        let (_dir, part) = temp("cancel");
        let cancel = AtomicBool::new(false);
        let e = download(
            &srv.url,
            &part,
            None,
            &cancel,
            &mut |_| cancel.store(true, Ordering::Relaxed),
            true,
        );
        assert!(matches!(e, Err(DownloadError::Cancelled)), "{e:?}");
        assert!(part.is_file());
    }

    #[test]
    fn a_larger_file_than_announced_is_rejected() {
        let srv = TestServer::start_ranged(body(), true, None);
        let (_dir, part) = temp("large");
        let no = AtomicBool::new(false);
        let e = download(&srv.url, &part, Some(1_000), &no, &mut |_| {}, true);
        assert!(matches!(e, Err(DownloadError::BadResponse(_))), "{e:?}");
        assert!(!part.exists());
    }

    #[test]
    fn missing_file_is_an_http_error() {
        let srv = TestServer::start(vec![(404, "nope".into())]);
        let (_dir, part) = temp("404");
        let no = AtomicBool::new(false);
        let e = download(&srv.url, &part, None, &no, &mut |_| {}, true);
        assert!(matches!(e, Err(DownloadError::Http(404))), "{e:?}");
    }
}
