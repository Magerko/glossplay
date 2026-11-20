//! Downloads and installs optional parts of the player (the speech recognizer, later the
//! voice engine) into `<data dir>/components/<id>/`, with the state of each install kept in
//! `component.json` inside that directory.
//!
//! An install never leaves a half-made component behind: everything is assembled in
//! `<id>.new` and swapped in at the end. Downloads go to `components/.downloads` as `.part`
//! files that survive a cancel or a crash and are resumed next time.

use crate::i18n::{Tr, t};
use crate::paths;
use crate::setup_text as text;
use gp_ai::download::{self, DownloadError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tests and debugging only: take files from this directory instead of the network, by the
/// last segment of their URL. Hashes are checked all the same.
pub const ENV_MIRROR: &str = "GLOSSPLAY_COMPONENT_MIRROR";
const STATE_FILE: &str = "component.json";
const DOWNLOADS: &str = ".downloads";
const RETRIES: u32 = 3;
const CHUNK: usize = 256 * 1024;
/// Room left over on the disk after an install, so it never fills the disk to the last byte.
const SPARE_SPACE: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Unpack {
    /// Kept as downloaded, at this path inside the component directory.
    Plain { path: String },
    /// A zip archive: entries under `from` whose path (without `from`) matches one of
    /// `files` go into `into`. `*` in a pattern stands for any run of characters except `/`.
    /// Every pattern without `*` must match, so a renamed binary is caught at install time.
    Zip {
        from: String,
        files: Vec<String>,
        into: String,
        unpacked_size: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileSpec {
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub unpack: Unpack,
    /// What the file is, in words: "speech model large-v3-turbo".
    pub label: String,
}

impl FileSpec {
    /// The last segment of the URL; also the name looked up in a mirror.
    pub fn file_name(&self) -> &str {
        let path = self.url.split(['?', '#']).next().unwrap_or("");
        path.rsplit('/').next().unwrap_or(path)
    }

    fn unpacked_size(&self) -> u64 {
        match &self.unpack {
            Unpack::Plain { .. } => self.size,
            Unpack::Zip { unpacked_size, .. } => *unpacked_size,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub id: String,
    pub name: String,
    /// Changes whenever any file changes; recorded in the state.
    pub version: String,
    pub files: Vec<FileSpec>,
    /// Licences and anything else to show before downloading.
    pub note: String,
    /// Component-specific facts kept in the state file, such as the chosen variant.
    pub meta: serde_json::Value,
}

impl Component {
    pub fn download_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn installed_size(&self) -> u64 {
        self.files.iter().map(FileSpec::unpacked_size).sum()
    }

    /// Peak disk use of a fresh install: archives sit next to what is unpacked from them
    /// until the end. Plain files are hard-linked, so they count once.
    pub fn space_needed(&self) -> u64 {
        self.files
            .iter()
            .map(|f| match f.unpack {
                Unpack::Plain { .. } => f.size,
                Unpack::Zip { unpacked_size, .. } => f.size + unpacked_size,
            })
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Relative to the component directory, with `/` separators.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// SHA-256 of the downloaded file this one came from.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub installed_at: u64,
    pub files: Vec<InstalledFile>,
    #[serde(default)]
    pub meta: serde_json::Value,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Installed {
    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative.split('/').collect::<PathBuf>())
    }

    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Preparing,
    Downloading,
    Verifying,
    Unpacking,
    Finishing,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    pub stage: Option<Stage>,
    pub file: String,
    pub file_done: u64,
    pub file_total: u64,
    /// Bytes of the component already on disk, counting resumed parts; out of `total`.
    pub done: u64,
    /// Bytes this install has to download, not counting files kept from before.
    pub total: u64,
    pub bytes_per_sec: f64,
    /// While downloading: how much of the current file was already there from an earlier,
    /// interrupted attempt.
    pub resumed_from: u64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        // Nothing to download reads as not started, not as done, until the very end.
        if self.total == 0 && self.stage != Some(Stage::Finishing) {
            return 0.0;
        }
        fraction(self.done, self.total)
    }

    pub fn file_fraction(&self) -> f32 {
        fraction(self.file_done, self.file_total)
    }
}

fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        return 1.0;
    }
    (done as f64 / total as f64).clamp(0.0, 1.0) as f32
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Cancelled => f.write_str(t(Tr::Cancelled)),
            InstallError::Failed(e) => f.write_str(e),
        }
    }
}

/// What a running install tells the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Progress(Progress),
    Done(Result<Installed, InstallError>),
}

/// Download speed over the last few seconds.
#[derive(Debug, Default)]
pub struct Rate {
    samples: std::collections::VecDeque<(f64, u64)>,
}

impl Rate {
    const WINDOW: f64 = 4.0;

    /// `bytes` is a running total; `secs` a monotonic clock.
    pub fn add(&mut self, secs: f64, bytes: u64) {
        if self.samples.back().is_some_and(|&(_, last)| bytes < last) {
            self.samples.clear();
        }
        self.samples.push_back((secs, bytes));
        while self
            .samples
            .front()
            .is_some_and(|&(t, _)| secs - t > Self::WINDOW)
            && self.samples.len() > 2
        {
            self.samples.pop_front();
        }
    }

    pub fn per_sec(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) if t1 - t0 >= 0.5 => (b1 - b0) as f64 / (t1 - t0),
            _ => 0.0,
        }
    }
}

pub fn root() -> PathBuf {
    paths::data_dir().join("components")
}

pub fn dir(id: &str) -> PathBuf {
    root().join(id)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn parse_state(json: &str) -> Option<Installed> {
    serde_json::from_str(json).ok()
}

fn read_state(dir: &Path) -> Option<Installed> {
    let mut state = parse_state(&std::fs::read_to_string(dir.join(STATE_FILE)).ok()?)?;
    state.dir = dir.to_owned();
    Some(state)
}

fn write_state(dir: &Path, state: &Installed) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let temp = dir.join(format!("{STATE_FILE}.tmp"));
    std::fs::write(&temp, json)?;
    std::fs::rename(temp, dir.join(STATE_FILE))
}

/// Whether every recorded file is there with its recorded size. Hashes were checked at
/// install time; comparing sizes is cheap enough to do on every use.
fn intact(state: &Installed) -> bool {
    !state.files.is_empty()
        && state.files.iter().all(|f| {
            std::fs::metadata(state.path(&f.path)).is_ok_and(|m| m.is_file() && m.len() == f.size)
        })
}

/// The installed component, if it is there and whole.
pub fn installed(id: &str) -> Option<Installed> {
    installed_in(&root(), id)
}

fn installed_in(root: &Path, id: &str) -> Option<Installed> {
    read_state(&root.join(id)).filter(|s| s.id == id && intact(s))
}

/// Changes the component-specific part of the state, such as "first run done".
pub fn update_meta(id: &str, change: impl FnOnce(&mut serde_json::Value)) -> Result<(), String> {
    let dir = dir(id);
    let mut state = read_state(&dir).ok_or_else(|| t(Tr::NotInstalled).to_owned())?;
    change(&mut state.meta);
    write_state(&dir, &state).map_err(|e| e.to_string())
}

/// Deletes the component. Downloads kept for resuming are left alone.
pub fn remove(id: &str) -> Result<(), String> {
    remove_in(&root(), id)
}

fn remove_in(root: &Path, id: &str) -> Result<(), String> {
    for name in [id.to_owned(), format!("{id}.new"), format!("{id}.old")] {
        match std::fs::remove_dir_all(root.join(&name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(text::cannot_remove(&e)),
        }
    }
    Ok(())
}

/// Free bytes on the disk holding `dir`, when the OS says.
pub fn free_space(dir: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        // The directory may not exist yet; its nearest existing ancestor is on the same disk.
        let existing = dir.ancestors().find(|d| d.is_dir())?;
        let wide: Vec<u16> = existing.as_os_str().encode_wide().chain([0]).collect();
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// Bytes this install would still need: what is not downloaded yet, plus room to unpack.
/// Files kept from the current install and finished downloads cost nothing.
pub fn space_still_needed(c: &Component) -> u64 {
    space_still_needed_in(&root(), c)
}

fn space_still_needed_in(root: &Path, c: &Component) -> u64 {
    let old = read_state(&root.join(&c.id));
    let downloads = root.join(DOWNLOADS);
    c.files
        .iter()
        .filter(|f| !old.as_ref().is_some_and(|o| reusable(o, f)))
        .map(|f| {
            let have = std::fs::metadata(downloaded_path(&downloads, f))
                .or_else(|_| std::fs::metadata(part_path(&downloads, f)))
                .map_or(0, |m| m.len().min(f.size));
            let unpack = match f.unpack {
                Unpack::Plain { .. } => 0,
                Unpack::Zip { unpacked_size, .. } => unpacked_size,
            };
            f.size - have + unpack
        })
        .sum::<u64>()
        + SPARE_SPACE
}

fn downloaded_path(downloads: &Path, f: &FileSpec) -> PathBuf {
    let short = f.sha256.get(..16).unwrap_or(&f.sha256);
    downloads.join(format!("{short}-{}", f.file_name()))
}

fn part_path(downloads: &Path, f: &FileSpec) -> PathBuf {
    let mut p = downloaded_path(downloads, f).into_os_string();
    p.push(".part");
    PathBuf::from(p)
}

/// Whether the current install already holds everything `f` would produce.
fn reusable(old: &Installed, f: &FileSpec) -> bool {
    let from_f: Vec<&InstalledFile> = old
        .files
        .iter()
        .filter(|i| i.source.eq_ignore_ascii_case(&f.sha256))
        .collect();
    !from_f.is_empty()
        && from_f.iter().all(|i| {
            std::fs::metadata(old.path(&i.path)).is_ok_and(|m| m.is_file() && m.len() == i.size)
        })
}

static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Marks a component directory as being installed into by this process while it lives.
struct BusyGuard(String);

impl BusyGuard {
    fn take(target: &Path) -> Option<BusyGuard> {
        let key = target.to_string_lossy().into_owned();
        let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        busy.get_or_insert_default()
            .insert(key.clone())
            .then(|| BusyGuard(key))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        if let Some(set) = BUSY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            set.remove(&self.0);
        }
    }
}

pub fn is_installing(id: &str) -> bool {
    BUSY.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.contains(dir(id).to_string_lossy().as_ref()))
}

/// Downloads, checks and installs `c`, replacing whatever version of it was there. Files the
/// current install already has are checked and kept instead of downloaded again. `report`
/// is called often; throttle on the receiving side.
pub fn install(
    c: &Component,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(&Progress),
) -> Result<Installed, InstallError> {
    let mirror = std::env::var_os(ENV_MIRROR)
        .filter(|m| !m.is_empty())
        .map(PathBuf::from);
    install_in(&root(), mirror.as_deref(), c, cancel, report)
}

struct Reporter<'a> {
    report: &'a mut dyn FnMut(&Progress),
    progress: Progress,
    rate: Rate,
    started: Instant,
}

impl Reporter<'_> {
    fn stage(&mut self, stage: Stage, file: &str, file_total: u64) {
        if stage != Stage::Downloading {
            self.progress.bytes_per_sec = 0.0;
        }
        self.progress.stage = Some(stage);
        self.progress.file = file.to_owned();
        self.progress.file_done = 0;
        self.progress.file_total = file_total;
        (self.report)(&self.progress);
    }

    fn file(&mut self, file_done: u64) {
        self.progress.file_done = file_done;
        (self.report)(&self.progress);
    }

    /// `base` is what earlier files of this install contributed to `done`.
    fn bytes(&mut self, base: u64, file_done: u64) {
        self.progress.file_done = file_done;
        self.progress.done = base + file_done;
        self.rate
            .add(self.started.elapsed().as_secs_f64(), self.progress.done);
        self.progress.bytes_per_sec = self.rate.per_sec();
        (self.report)(&self.progress);
    }
}

fn failed(e: impl Into<String>) -> InstallError {
    InstallError::Failed(e.into())
}

fn install_in(
    root: &Path,
    mirror: Option<&Path>,
    c: &Component,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(&Progress),
) -> Result<Installed, InstallError> {
    let target = root.join(&c.id);
    let _busy = BusyGuard::take(&target).ok_or_else(|| failed(t(Tr::AlreadyInstalling)))?;
    let staging = root.join(format!("{}.new", c.id));
    let downloads = root.join(DOWNLOADS);
    std::fs::create_dir_all(&downloads).map_err(|e| failed(text::cannot_write(&downloads, &e)))?;
    let mut rep = Reporter {
        report,
        progress: Progress::default(),
        rate: Rate::default(),
        started: Instant::now(),
    };
    rep.stage(Stage::Preparing, "", 0);

    // Files of the current install that this one also needs are checked byte for byte:
    // a reinstall is often meant to repair something.
    let old = read_state(&target).filter(|s| s.id == c.id);
    let mut keep = vec![false; c.files.len()];
    if let Some(old) = &old {
        for (i, f) in c.files.iter().enumerate() {
            if reusable(old, f) {
                keep[i] = verify_kept(old, f, cancel, &mut rep)?;
            }
        }
    }

    let fetch: Vec<&FileSpec> = c
        .files
        .iter()
        .zip(&keep)
        .filter(|(_, k)| !**k)
        .map(|(f, _)| f)
        .collect();
    rep.progress.total = fetch.iter().map(|f| f.size).sum();
    let needed = space_still_needed_in(root, c);
    if let Some(free) = free_space(root)
        && free < needed
    {
        return Err(failed(text::not_enough_space(needed, free, root)));
    }

    let mut fetched = Vec::new();
    let mut base = 0;
    for f in &fetch {
        fetched.push(fetch_file(f, &downloads, mirror, cancel, &mut rep, base)?);
        base += f.size;
    }

    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed(text::cannot_write(&staging, &e))),
    }
    std::fs::create_dir_all(&staging).map_err(|e| failed(text::cannot_write(&staging, &e)))?;
    let result =
        assemble(c, old.as_ref(), &keep, &fetched, &staging, cancel, &mut rep).and_then(|files| {
            rep.stage(Stage::Finishing, "", 0);
            let mut state = Installed {
                id: c.id.clone(),
                version: c.version.clone(),
                installed_at: now(),
                files,
                meta: c.meta.clone(),
                dir: target.clone(),
            };
            write_state(&staging, &state).map_err(|e| failed(text::cannot_write(&staging, &e)))?;
            swap(root, &c.id, &staging, &target)?;
            state.dir = target.clone();
            Ok(state)
        });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return result;
    }
    for (f, path) in fetch.iter().zip(&fetched) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(part_path(&downloads, f));
    }
    result
}

fn verify_kept(
    old: &Installed,
    f: &FileSpec,
    cancel: &AtomicBool,
    rep: &mut Reporter,
) -> Result<bool, InstallError> {
    for i in old
        .files
        .iter()
        .filter(|i| i.source.eq_ignore_ascii_case(&f.sha256))
    {
        rep.stage(Stage::Verifying, &i.path, i.size);
        let hash = sha256_file(&old.path(&i.path), cancel, &mut |n| rep.file(n))?;
        if !hash.eq_ignore_ascii_case(&i.sha256) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Brings one file into `downloads`, checked against its hash, and returns its path there.
fn fetch_file(
    f: &FileSpec,
    downloads: &Path,
    mirror: Option<&Path>,
    cancel: &AtomicBool,
    rep: &mut Reporter,
    base: u64,
) -> Result<PathBuf, InstallError> {
    let done = downloaded_path(downloads, f);
    // Only ever renamed to this name after its hash matched.
    if std::fs::metadata(&done).is_ok_and(|m| m.len() == f.size) {
        rep.progress.resumed_from = f.size;
        rep.stage(Stage::Downloading, f.file_name(), f.size);
        rep.bytes(base, f.size);
        return Ok(done);
    }
    let part = part_path(downloads, f);
    rep.progress.resumed_from = std::fs::metadata(&part).map_or(0, |m| m.len().min(f.size));
    rep.stage(Stage::Downloading, f.file_name(), f.size);
    let mut attempt = 0;
    loop {
        let mut on_bytes = |n: u64| rep.bytes(base, n);
        let result = match mirror {
            Some(m) => copy_from_mirror(&m.join(f.file_name()), &part, cancel, &mut on_bytes),
            None => download::download_to_file(&f.url, &part, Some(f.size), cancel, &mut on_bytes),
        };
        match result {
            Ok(_) => break,
            Err(DownloadError::Cancelled) => return Err(InstallError::Cancelled),
            Err(e) if e.is_transient() && attempt < RETRIES => {
                attempt += 1;
                // A dropped connection is common on long downloads; the part is kept and
                // the next attempt continues from where this one stopped.
                let until = Instant::now() + Duration::from_secs(2 * u64::from(attempt));
                while Instant::now() < until {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(InstallError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(e) => return Err(failed(text::download_failed(&f.label, &e))),
        }
    }
    rep.stage(Stage::Verifying, f.file_name(), f.size);
    let size = std::fs::metadata(&part).map_or(0, |m| m.len());
    let hash = if size == f.size {
        Some(sha256_file(&part, cancel, &mut |n| rep.file(n))?)
    } else {
        None
    };
    if !hash.is_some_and(|h| h.eq_ignore_ascii_case(&f.sha256)) {
        let _ = std::fs::remove_file(&part);
        return Err(failed(text::damaged_download(&f.label)));
    }
    std::fs::rename(&part, &done).map_err(|e| failed(text::cannot_write(&done, &e)))?;
    Ok(done)
}

/// Copies a mirrored file the way a download would go, resuming a part file the same way.
fn copy_from_mirror(
    source: &Path,
    part: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, DownloadError> {
    let mut src = File::open(source).map_err(|e| {
        DownloadError::BadResponse(format!("{} is not in the mirror: {e}", source.display()))
    })?;
    let total = src.metadata().map_err(DownloadError::Disk)?.len();
    let mut have = std::fs::metadata(part).map_or(0, |m| m.len());
    if have > total {
        have = 0;
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(part)
        .map_err(DownloadError::Disk)?;
    src.seek(SeekFrom::Start(have))
        .map_err(DownloadError::Disk)?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = have;
    progress(done);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = src.read(&mut buf).map_err(DownloadError::Disk)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(DownloadError::Disk)?;
        done += n as u64;
        progress(done);
    }
    out.sync_all().map_err(DownloadError::Disk)?;
    Ok(done)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<String, InstallError> {
    let mut file = File::open(path).map_err(|e| failed(text::cannot_read(path, &e)))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(InstallError::Cancelled);
        }
        let n = file
            .read(&mut buf)
            .map_err(|e| failed(text::cannot_read(path, &e)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Puts every file of the component into `staging` and lists them.
fn assemble(
    c: &Component,
    old: Option<&Installed>,
    keep: &[bool],
    fetched: &[PathBuf],
    staging: &Path,
    cancel: &AtomicBool,
    rep: &mut Reporter,
) -> Result<Vec<InstalledFile>, InstallError> {
    let mut files = Vec::new();
    let mut fetched = fetched.iter();
    for (f, &kept) in c.files.iter().zip(keep) {
        if kept {
            let old = old.expect("kept files come from an install");
            for i in old
                .files
                .iter()
                .filter(|i| i.source.eq_ignore_ascii_case(&f.sha256))
            {
                link_or_copy(&old.path(&i.path), &staging.join(&i.path))?;
                files.push(i.clone());
            }
            continue;
        }
        let path = fetched.next().expect("one fetched file per file not kept");
        match &f.unpack {
            Unpack::Plain { path: rel } => {
                let rel = safe_relative(rel).ok_or_else(|| failed(t(Tr::UnsafePath)))?;
                link_or_copy(path, &staging.join(&rel))?;
                files.push(InstalledFile {
                    path: rel,
                    size: f.size,
                    sha256: f.sha256.to_ascii_lowercase(),
                    source: f.sha256.to_ascii_lowercase(),
                });
            }
            Unpack::Zip {
                from,
                files: patterns,
                into,
                unpacked_size,
            } => {
                rep.stage(Stage::Unpacking, f.file_name(), *unpacked_size);
                files.extend(unzip(
                    path,
                    from,
                    patterns,
                    into,
                    &f.sha256,
                    staging,
                    cancel,
                    &mut |n| rep.file(n),
                )?);
            }
        }
    }
    Ok(files)
}

fn link_or_copy(from: &Path, to: &Path) -> Result<(), InstallError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| failed(text::cannot_write(parent, &e)))?;
    }
    // A hard link costs no space and no time; a copy is the fallback for file systems
    // without links.
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|e| failed(text::cannot_write(to, &e)))
}

/// A relative path that stays inside the directory it is joined to, with `/` separators.
/// `None` for absolute paths, drive letters and `..`.
pub fn safe_relative(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.contains(':') || path.contains('\0') {
        return None;
    }
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    Some(parts.join("/"))
}

/// `*` matches any run of characters except `/`; letters compare without case.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ni));
            pi += 1;
        } else if pi < p.len() && p[pi].eq_ignore_ascii_case(&n[ni]) {
            pi += 1;
            ni += 1;
        } else if let Some((sp, sn)) = star.filter(|&(_, sn)| n[sn] != b'/') {
            star = Some((sp, sn + 1));
            pi = sp + 1;
            ni = sn + 1;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Where an archive entry goes, relative to the `into` directory; `None` to skip it.
/// An entry that would land outside the component is an error for the whole archive.
pub fn select_entry(name: &str, from: &str, patterns: &[String]) -> Result<Option<String>, ()> {
    let Some(rel) = safe_relative(name) else {
        return Err(());
    };
    let from = from.trim_matches('/');
    let rest = if from.is_empty() {
        rel.as_str()
    } else {
        match rel.strip_prefix(from).and_then(|r| r.strip_prefix('/')) {
            Some(r) => r,
            None => return Ok(None),
        }
    };
    Ok(patterns
        .iter()
        .any(|p| glob_match(p, rest))
        .then(|| rest.to_owned()))
}

#[allow(clippy::too_many_arguments)]
fn unzip(
    archive: &Path,
    from: &str,
    patterns: &[String],
    into: &str,
    source: &str,
    staging: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Vec<InstalledFile>, InstallError> {
    let bad = |e: &dyn std::fmt::Display| failed(text::bad_archive(archive, e));
    let file = File::open(archive).map_err(|e| failed(text::cannot_read(archive, &e)))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
    let into = if into.is_empty() {
        String::new()
    } else {
        safe_relative(into).ok_or_else(|| failed(t(Tr::UnsafePath)))?
    };
    let mut out_files = Vec::new();
    let mut matched = vec![false; patterns.len()];
    let mut written = 0;
    let mut buf = vec![0u8; CHUNK];
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| bad(&e))?;
        let name = entry.name().map_err(|e| bad(&e))?.into_owned();
        let rel = select_entry(&name, from, patterns).map_err(|()| failed(t(Tr::UnsafePath)))?;
        let Some(rel) = rel.filter(|_| !entry.is_dir()) else {
            continue;
        };
        if entry.is_symlink() {
            return Err(failed(t(Tr::UnsafePath)));
        }
        for (m, p) in matched.iter_mut().zip(patterns) {
            *m |= glob_match(p, &rel);
        }
        let rel = if into.is_empty() {
            rel
        } else {
            format!("{into}/{rel}")
        };
        let target = staging.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed(text::cannot_write(parent, &e)))?;
        }
        let mut out = File::create(&target).map_err(|e| failed(text::cannot_write(&target, &e)))?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(InstallError::Cancelled);
            }
            let n = entry.read(&mut buf).map_err(|e| bad(&e))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| failed(text::cannot_write(&target, &e)))?;
            hasher.update(&buf[..n]);
            size += n as u64;
            written += n as u64;
            progress(written);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if entry.unix_mode().is_some_and(|m| m & 0o111 != 0) {
                let _ = out.set_permissions(std::fs::Permissions::from_mode(0o755));
            }
        }
        out_files.push(InstalledFile {
            path: rel,
            size,
            sha256: to_hex(&hasher.finalize()),
            source: source.to_ascii_lowercase(),
        });
    }
    let missing: Vec<&str> = patterns
        .iter()
        .zip(&matched)
        .filter(|(p, m)| !**m && !p.contains('*'))
        .map(|(p, _)| p.as_str())
        .collect();
    if !missing.is_empty() || out_files.is_empty() {
        return Err(failed(text::archive_lacks(archive, &missing)));
    }
    Ok(out_files)
}

/// Replaces `target` with `staging`, putting the old one back if that fails.
fn swap(root: &Path, id: &str, staging: &Path, target: &Path) -> Result<(), InstallError> {
    let old = root.join(format!("{id}.old"));
    let _ = std::fs::remove_dir_all(&old);
    let had_old = target.exists();
    if had_old {
        std::fs::rename(target, &old).map_err(|e| failed(text::in_use(target, &e)))?;
    }
    if let Err(e) = std::fs::rename(staging, target) {
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        return Err(failed(text::cannot_write(target, &e)));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn hex(bytes: &[u8]) -> String {
        to_hex(&Sha256::digest(bytes))
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    struct Fixture {
        root: PathBuf,
        mirror: PathBuf,
        _base: gp_core::scratch::TempDir,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let base = gp_core::scratch::TempDir::new(&format!("gp-comp-{name}"));
            let (root, mirror) = (base.join("components"), base.join("mirror"));
            std::fs::create_dir_all(&mirror).unwrap();
            Fixture {
                root,
                mirror,
                _base: base,
            }
        }

        fn put(&self, name: &str, data: &[u8]) -> FileSpec {
            std::fs::write(self.mirror.join(name), data).unwrap();
            FileSpec {
                url: format!("https://example.com/files/{name}"),
                size: data.len() as u64,
                sha256: hex(data),
                unpack: Unpack::Plain {
                    path: format!("models/{name}"),
                },
                label: name.into(),
            }
        }

        fn install(&self, c: &Component, cancel: &AtomicBool) -> Result<Installed, InstallError> {
            install_in(&self.root, Some(&self.mirror), c, cancel, &mut |_| {})
        }
    }

    fn engine_zip(fx: &Fixture) -> FileSpec {
        let data = zip_of(&[
            ("Release/server.exe", b"server binary"),
            ("Release/ggml-cpu.dll", b"cpu"),
            ("Release/ggml-cuda.dll", b"cuda"),
            ("Release/unrelated.exe", b"not wanted"),
            ("README.txt", b"readme"),
        ]);
        let mut spec = fx.put("engine.zip", &data);
        spec.unpack = Unpack::Zip {
            from: "Release".into(),
            files: vec!["server.exe".into(), "ggml*.dll".into()],
            into: "engine".into(),
            unpacked_size: 20,
        };
        spec
    }

    fn component(files: Vec<FileSpec>) -> Component {
        Component {
            id: "test".into(),
            name: "Test".into(),
            version: "1".into(),
            files,
            note: String::new(),
            meta: serde_json::json!({ "variant": "cpu" }),
        }
    }

    #[test]
    fn installs_plain_files_and_zip_entries() {
        let fx = Fixture::new("install");
        let model = fx.put("model.bin", b"model weights");
        let c = component(vec![engine_zip(&fx), model]);
        let no = AtomicBool::new(false);
        let mut last = Progress::default();
        let state = install_in(&fx.root, Some(&fx.mirror), &c, &no, &mut |p| {
            last = p.clone()
        })
        .unwrap();
        let mut paths: Vec<&str> = state.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "engine/ggml-cpu.dll",
                "engine/ggml-cuda.dll",
                "engine/server.exe",
                "models/model.bin"
            ]
        );
        let dir = fx.root.join("test");
        assert_eq!(
            std::fs::read(dir.join("engine/server.exe")).unwrap(),
            b"server binary"
        );
        assert_eq!(
            std::fs::read(dir.join("models/model.bin")).unwrap(),
            b"model weights"
        );
        assert!(!dir.join("engine/unrelated.exe").exists());
        assert_eq!(last.stage, Some(Stage::Finishing));
        assert_eq!(last.done, last.total);
        let back = installed_in(&fx.root, "test").unwrap();
        assert_eq!(back.meta["variant"], "cpu");
        assert_eq!(back.files, state.files);
        // Nothing is left in the download area once the install is done.
        let leftovers = std::fs::read_dir(fx.root.join(DOWNLOADS)).unwrap().count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_damaged_download_installs_nothing_and_is_deleted() {
        let fx = Fixture::new("damaged");
        let mut model = fx.put("model.bin", b"model weights");
        model.sha256 = hex(b"other weights");
        let c = component(vec![model.clone()]);
        let no = AtomicBool::new(false);
        let e = fx.install(&c, &no).unwrap_err();
        assert!(
            matches!(&e, InstallError::Failed(t) if t.contains("model.bin")),
            "{e:?}"
        );
        assert!(installed_in(&fx.root, "test").is_none());
        assert!(!fx.root.join("test").exists());
        let downloads = fx.root.join(DOWNLOADS);
        assert!(!part_path(&downloads, &model).exists());
        assert!(!downloaded_path(&downloads, &model).exists());
    }

    #[test]
    fn cancel_keeps_the_part_and_the_next_run_resumes_it() {
        let fx = Fixture::new("cancel");
        let data = vec![7u8; 3 * CHUNK + 10];
        let model = fx.put("big.bin", &data);
        let c = component(vec![model.clone()]);
        let cancel = AtomicBool::new(false);
        let e = install_in(&fx.root, Some(&fx.mirror), &c, &cancel, &mut |p| {
            if p.stage == Some(Stage::Downloading) && p.file_done > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(e, InstallError::Cancelled);
        let part = part_path(&fx.root.join(DOWNLOADS), &model);
        let kept = std::fs::metadata(&part).unwrap().len();
        assert!(kept > 0 && kept < data.len() as u64);
        assert!(installed_in(&fx.root, "test").is_none());

        let no = AtomicBool::new(false);
        let mut first = None;
        install_in(&fx.root, Some(&fx.mirror), &c, &no, &mut |p| {
            if p.stage == Some(Stage::Downloading) {
                first.get_or_insert(p.resumed_from);
            }
        })
        .unwrap();
        assert_eq!(first, Some(kept));
        assert!(installed_in(&fx.root, "test").is_some());
    }

    #[test]
    fn reinstall_keeps_checked_files_and_replaces_damaged_ones() {
        let fx = Fixture::new("reuse");
        let engine = engine_zip(&fx);
        let model = fx.put("model.bin", b"model weights");
        let c = component(vec![engine.clone(), model.clone()]);
        let no = AtomicBool::new(false);
        fx.install(&c, &no).unwrap();

        // Gone from the mirror: a second install must not need it.
        std::fs::remove_file(fx.mirror.join("engine.zip")).unwrap();
        let small = fx.put("small.bin", b"smaller model");
        let c2 = component(vec![engine.clone(), small]);
        let state = fx.install(&c2, &no).unwrap();
        let dir = fx.root.join("test");
        assert!(dir.join("engine/server.exe").is_file());
        assert!(dir.join("models/small.bin").is_file());
        assert!(!dir.join("models/model.bin").exists());
        assert_eq!(state.files.len(), 4);

        // Same size, different bytes: the check notices and the file is fetched again.
        std::fs::write(dir.join("models/small.bin"), b"smaller m0del").unwrap();
        fx.install(&c2, &no).unwrap();
        assert_eq!(
            std::fs::read(dir.join("models/small.bin")).unwrap(),
            b"smaller model"
        );
    }

    #[test]
    fn archives_reaching_outside_are_refused() {
        let fx = Fixture::new("slip");
        let data = zip_of(&[
            ("Release/server.exe", b"ok"),
            ("Release/../../evil.dll", b"x"),
        ]);
        let mut spec = fx.put("slip.zip", &data);
        spec.unpack = Unpack::Zip {
            from: "Release".into(),
            files: vec!["*".into()],
            into: "engine".into(),
            unpacked_size: 3,
        };
        let no = AtomicBool::new(false);
        let e = fx.install(&component(vec![spec]), &no).unwrap_err();
        assert_eq!(e, InstallError::Failed(t(Tr::UnsafePath).into()));
        assert!(!fx.root.join("test").exists());
        assert!(!fx.root.join("evil.dll").exists());
        assert!(!fx.root.parent().unwrap().join("evil.dll").exists());
    }

    #[test]
    fn an_archive_without_the_binary_is_refused() {
        let fx = Fixture::new("lacks");
        let data = zip_of(&[("Release/ggml.dll", b"x")]);
        let mut spec = fx.put("lacks.zip", &data);
        spec.unpack = Unpack::Zip {
            from: "Release".into(),
            files: vec!["server.exe".into(), "ggml*.dll".into()],
            into: "engine".into(),
            unpacked_size: 1,
        };
        let no = AtomicBool::new(false);
        let e = fx.install(&component(vec![spec]), &no).unwrap_err();
        assert!(
            matches!(&e, InstallError::Failed(t) if t.contains("server.exe")),
            "{e:?}"
        );
    }

    #[test]
    fn remove_deletes_the_component() {
        let fx = Fixture::new("remove");
        let c = component(vec![fx.put("model.bin", b"m")]);
        fx.install(&c, &AtomicBool::new(false)).unwrap();
        remove_in(&fx.root, "test").unwrap();
        assert!(!fx.root.join("test").exists());
        assert!(installed_in(&fx.root, "test").is_none());
        remove_in(&fx.root, "test").unwrap();
    }

    #[test]
    fn a_missing_or_resized_file_means_not_installed() {
        let fx = Fixture::new("intact");
        let c = component(vec![fx.put("model.bin", b"model")]);
        fx.install(&c, &AtomicBool::new(false)).unwrap();
        std::fs::write(fx.root.join("test/models/model.bin"), b"mod").unwrap();
        assert!(installed_in(&fx.root, "test").is_none());
    }

    #[test]
    fn relative_paths() {
        assert_eq!(safe_relative("a/b.dll").as_deref(), Some("a/b.dll"));
        assert_eq!(safe_relative("a\\b.dll").as_deref(), Some("a/b.dll"));
        assert_eq!(safe_relative("./a//b").as_deref(), Some("a/b"));
        for bad in [
            "../x",
            "a/../../x",
            "a/..",
            "/etc/passwd",
            "\\x",
            "C:/Windows/x.dll",
            "c:x",
            "",
            ".",
        ] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
    }

    #[test]
    fn entry_selection() {
        let pats = vec!["whisper-server.exe".to_owned(), "ggml*.dll".to_owned()];
        let sel = |n: &str| select_entry(n, "Release", &pats);
        assert_eq!(
            sel("Release/whisper-server.exe"),
            Ok(Some("whisper-server.exe".into()))
        );
        assert_eq!(
            sel("Release/ggml-cuda.dll"),
            Ok(Some("ggml-cuda.dll".into()))
        );
        assert_eq!(sel("Release/llama.dll"), Ok(None));
        assert_eq!(sel("Other/ggml.dll"), Ok(None));
        assert_eq!(sel("Releases/ggml.dll"), Ok(None));
        assert_eq!(sel("Release/sub/ggml.dll"), Ok(None));
        assert_eq!(sel("Release/../ggml.dll"), Err(()));
        assert_eq!(sel("/Release/ggml.dll"), Err(()));
    }

    #[test]
    fn wildcards() {
        assert!(glob_match("ggml*.dll", "ggml.dll"));
        assert!(glob_match("ggml*.dll", "ggml-cpu-alderlake.dll"));
        assert!(glob_match("GGML*.DLL", "ggml-base.dll"));
        assert!(glob_match("cublas*.dll", "cublasLt64_12.dll"));
        assert!(!glob_match("cublas*.dll", "nvblas64_12.dll"));
        assert!(!glob_match("ggml*.dll", "ggml.dll.bak"));
        assert!(!glob_match("*.dll", "sub/x.dll"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("a", "ab"));
    }

    #[test]
    fn state_round_trip_and_bad_state() {
        let s = Installed {
            id: "whisper".into(),
            version: "v1".into(),
            installed_at: 5,
            files: vec![InstalledFile {
                path: "models/m.bin".into(),
                size: 3,
                sha256: "ab".into(),
                source: "ab".into(),
            }],
            meta: serde_json::json!({ "engine": "cpu" }),
            dir: PathBuf::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(parse_state(&json), Some(s));
        assert_eq!(parse_state("{"), None);
        assert_eq!(parse_state(r#"{"id":"x"}"#), None);
        let no_meta = r#"{"id":"x","version":"1","installed_at":1,"files":[]}"#;
        assert_eq!(parse_state(no_meta).unwrap().meta, serde_json::Value::Null);
    }

    #[test]
    fn progress_fractions() {
        let p = Progress {
            done: 250,
            total: 1000,
            file_done: 5,
            file_total: 10,
            ..Default::default()
        };
        assert_eq!(p.fraction(), 0.25);
        assert_eq!(p.file_fraction(), 0.5);
        let nothing = Progress::default();
        assert_eq!(nothing.fraction(), 0.0);
        let finished = Progress {
            stage: Some(Stage::Finishing),
            ..Default::default()
        };
        assert_eq!(finished.fraction(), 1.0);
        let over = Progress {
            done: 2,
            total: 1,
            ..Default::default()
        };
        assert_eq!(over.fraction(), 1.0);
    }

    #[test]
    fn download_rate() {
        let mut r = Rate::default();
        assert_eq!(r.per_sec(), 0.0);
        r.add(0.0, 0);
        r.add(0.2, 100);
        assert_eq!(r.per_sec(), 0.0, "too short to tell");
        r.add(1.0, 1000);
        assert_eq!(r.per_sec(), 1000.0);
        for s in 2..=10 {
            r.add(s as f64, 1000 + (s as u64 - 1) * 10);
        }
        assert!((r.per_sec() - 10.0).abs() < 1e-9, "{}", r.per_sec());
        r.add(11.0, 5);
        r.add(12.0, 105);
        assert_eq!(r.per_sec(), 100.0, "a restart from zero starts over");
    }

    #[test]
    fn sizes() {
        let c = component(vec![
            FileSpec {
                url: "https://x/a.zip".into(),
                size: 10,
                sha256: "a".into(),
                unpack: Unpack::Zip {
                    from: String::new(),
                    files: vec![],
                    into: String::new(),
                    unpacked_size: 30,
                },
                label: String::new(),
            },
            FileSpec {
                url: "https://x/resolve/abc/m.bin?download=1".into(),
                size: 5,
                sha256: "b".into(),
                unpack: Unpack::Plain { path: "m".into() },
                label: String::new(),
            },
        ]);
        assert_eq!(c.download_size(), 15);
        assert_eq!(c.installed_size(), 35);
        assert_eq!(c.space_needed(), 45);
        assert_eq!(c.files[1].file_name(), "m.bin");
    }
}
