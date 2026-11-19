//! A thin libmpv binding loaded at run time, so the build needs no import library
//! and the same code works with any toolchain on any OS.

use crate::MediaError;
use libloading::Library;
use std::ffi::OsStr;
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

#[repr(C)]
struct RawEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RawEndFile {
    reason: c_int,
    error: c_int,
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct OpenGlInitParams {
    get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;

const PARAM_API_TYPE: c_int = 1;
const PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const PARAM_OPENGL_FBO: c_int = 3;
const PARAM_FLIP_Y: c_int = 4;

macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            _lib: Library,
        }

        impl Api {
            unsafe fn load(lib: Library) -> Result<Api, libloading::Error> {
                unsafe {
                    Ok(Api {
                        $($name: *lib.get(concat!("mpv_", stringify!($name), "\0").as_bytes())?,)*
                        _lib: lib,
                    })
                }
            }
        }
    };
}

api! {
    create: fn() -> *mut c_void;
    initialize: fn(*mut c_void) -> c_int;
    terminate_destroy: fn(*mut c_void);
    set_option_string: fn(*mut c_void, *const c_char, *const c_char) -> c_int;
    set_property_string: fn(*mut c_void, *const c_char, *const c_char) -> c_int;
    get_property_string: fn(*mut c_void, *const c_char) -> *mut c_char;
    get_property: fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
    set_property: fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
    command: fn(*mut c_void, *const *const c_char) -> c_int;
    command_async: fn(*mut c_void, u64, *const *const c_char) -> c_int;
    wait_event: fn(*mut c_void, c_double) -> *mut RawEvent;
    error_string: fn(c_int) -> *const c_char;
    free: fn(*mut c_void);
    render_context_create: fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int;
    render_context_set_update_callback: fn(*mut c_void, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void);
    render_context_update: fn(*mut c_void) -> u64;
    render_context_render: fn(*mut c_void, *mut RenderParam) -> c_int;
    render_context_report_swap: fn(*mut c_void);
    render_context_free: fn(*mut c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    FileLoaded,
    /// Playback of the file ended; `error` is true when it failed to open or decode.
    EndFile {
        error: bool,
    },
    Shutdown,
    Other,
}

pub struct Mpv {
    api: Api,
    handle: *mut c_void,
}

// libmpv's client API is documented as thread-safe.
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

fn library_candidates(configured: &str) -> Vec<String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned));
    candidates_for(
        configured,
        exe_dir.as_deref(),
        std::env::var_os("PATH").as_deref(),
        cfg!(windows),
    )
}

/// The environment is passed in so the search order can be tested.
fn candidates_for(
    configured: &str,
    exe_dir: Option<&Path>,
    path_var: Option<&OsStr>,
    windows: bool,
) -> Vec<String> {
    if !configured.is_empty() {
        return vec![configured.to_owned()];
    }
    let names: &[&str] = if windows {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &[
            "libmpv.2.dylib",
            "libmpv.dylib",
            "/opt/homebrew/lib/libmpv.dylib",
            "/usr/local/lib/libmpv.dylib",
        ]
    } else {
        &["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
    };
    let mut out = Vec::new();
    if windows {
        // A bare DLL name makes Windows look in the current directory, where a planted
        // library would be loaded, so I only try full paths.
        let path_dirs = path_var.map(std::env::split_paths).into_iter().flatten();
        let dirs: Vec<PathBuf> = exe_dir
            .map(Path::to_owned)
            .into_iter()
            .chain(path_dirs)
            .filter(|d| d.is_absolute())
            .collect();
        for name in names {
            for dir in &dirs {
                out.push(dir.join(name).to_string_lossy().into_owned());
            }
        }
        return out;
    }
    for name in names {
        if let Some(dir) = exe_dir {
            out.push(dir.join(name).to_string_lossy().into_owned());
        }
        out.push((*name).to_owned());
    }
    out
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("interior NULs were removed")
}

impl Mpv {
    /// `options` are set before initialisation (`vo`, `hwdec`, …).
    pub fn new(library_path: &str, options: &[(&str, &str)]) -> Result<Mpv, MediaError> {
        let tried = library_candidates(library_path);
        let api = tried
            .iter()
            .find_map(|path| unsafe { Library::new(path).ok().and_then(|lib| Api::load(lib).ok()) })
            .ok_or_else(|| {
                // The error text only counts the PATH folders; the whole list helps debugging.
                eprintln!("libmpv was not found; looked in: {}", tried.join(", "));
                MediaError::LibraryNotFound {
                    tried: tried.clone(),
                }
            })?;
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err(MediaError::Mpv {
                code: -1,
                what: "mpv_create".into(),
            });
        }
        let mpv = Mpv { api, handle };
        for (name, value) in options {
            let code = unsafe {
                (mpv.api.set_option_string)(handle, cstr(name).as_ptr(), cstr(value).as_ptr())
            };
            mpv.check(code, &format!("option {name}={value}"))?;
        }
        let code = unsafe { (mpv.api.initialize)(handle) };
        mpv.check(code, "mpv_initialize")?;
        Ok(mpv)
    }

    fn check(&self, code: c_int, what: &str) -> Result<(), MediaError> {
        if code >= 0 {
            return Ok(());
        }
        let msg = unsafe { CStr::from_ptr((self.api.error_string)(code)) }.to_string_lossy();
        Err(MediaError::Mpv {
            code,
            what: format!("{what}: {msg}"),
        })
    }

    pub fn command(&self, args: &[&str]) -> Result<(), MediaError> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        let code = unsafe { (self.api.command)(self.handle, ptrs.as_ptr()) };
        self.check(code, &args.join(" "))
    }

    /// Queues a command without waiting for it; used from the UI thread for seeks.
    pub fn command_async(&self, args: &[&str]) {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        unsafe { (self.api.command_async)(self.handle, 0, ptrs.as_ptr()) };
    }

    pub fn set(&self, name: &str, value: &str) {
        unsafe {
            (self.api.set_property_string)(self.handle, cstr(name).as_ptr(), cstr(value).as_ptr())
        };
    }

    pub fn set_flag(&self, name: &str, value: bool) {
        let mut v: c_int = value.into();
        unsafe {
            (self.api.set_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_FLAG,
                (&raw mut v).cast(),
            );
        }
    }

    pub fn set_f64(&self, name: &str, value: f64) {
        let mut v = value;
        unsafe {
            (self.api.set_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_DOUBLE,
                (&raw mut v).cast(),
            );
        }
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        unsafe {
            let p = (self.api.get_property_string)(self.handle, cstr(name).as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.api.free)(p.cast());
            Some(s)
        }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let mut v: c_double = 0.0;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_DOUBLE,
                (&raw mut v).cast(),
            )
        };
        (code >= 0).then_some(v)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let mut v: i64 = 0;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_INT64,
                (&raw mut v).cast(),
            )
        };
        (code >= 0).then_some(v)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let mut v: c_int = 0;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_FLAG,
                (&raw mut v).cast(),
            )
        };
        (code >= 0).then_some(v != 0)
    }

    /// Returns the next pending event without blocking, or None when the queue is empty.
    pub fn poll_event(&self) -> Option<Event> {
        let ev = unsafe { &*(self.api.wait_event)(self.handle, 0.0) };
        match ev.event_id {
            0 => None,
            1 => Some(Event::Shutdown),
            7 => {
                // mpv_end_file_reason: 4 is MPV_END_FILE_REASON_ERROR.
                let error =
                    !ev.data.is_null() && unsafe { (*ev.data.cast::<RawEndFile>()).reason } == 4;
                Some(Event::EndFile { error })
            }
            8 => Some(Event::FileLoaded),
            _ => Some(Event::Other),
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.handle) };
    }
}

pub type ProcLoader = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;
type UpdateFn = Box<dyn Fn() + Send + Sync>;

unsafe extern "C" fn proc_trampoline(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { (*ctx.cast::<ProcLoader>())(CStr::from_ptr(name)).cast_mut() }
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    unsafe { (*ctx.cast::<UpdateFn>())() }
}

/// mpv's OpenGL renderer. Create, use and `destroy` it on the thread that owns the GL context.
pub struct RenderContext {
    mpv: Arc<Mpv>,
    ctx: AtomicPtr<c_void>,
    // Boxed so the pointers handed to mpv stay valid when this struct moves.
    _loader: Box<ProcLoader>,
    _on_update: Box<UpdateFn>,
}

// egui paint callbacks must be Send + Sync, but they only ever run on the GL thread.
unsafe impl Send for RenderContext {}
unsafe impl Sync for RenderContext {}

impl RenderContext {
    /// `on_update` is called from an mpv thread whenever a new frame is ready.
    pub fn new(
        mpv: Arc<Mpv>,
        loader: ProcLoader,
        on_update: UpdateFn,
    ) -> Result<RenderContext, MediaError> {
        let loader = Box::new(loader);
        let on_update = Box::new(on_update);
        let mut init = OpenGlInitParams {
            get_proc_address: proc_trampoline,
            get_proc_address_ctx: (&raw const *loader).cast_mut().cast(),
        };
        let mut params = [
            RenderParam {
                kind: PARAM_API_TYPE,
                data: c"opengl".as_ptr().cast_mut().cast(),
            },
            RenderParam {
                kind: PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast(),
            },
            RenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        let mut ctx = ptr::null_mut();
        let code =
            unsafe { (mpv.api.render_context_create)(&mut ctx, mpv.handle, params.as_mut_ptr()) };
        mpv.check(code, "mpv_render_context_create")?;
        unsafe {
            (mpv.api.render_context_set_update_callback)(
                ctx,
                Some(update_trampoline),
                (&raw const *on_update).cast_mut().cast(),
            );
        }
        Ok(RenderContext {
            mpv,
            ctx: AtomicPtr::new(ctx),
            _loader: loader,
            _on_update: on_update,
        })
    }

    /// Draws the current frame into `fbo`.
    pub fn render(&self, fbo: i32, width: i32, height: i32, flip_y: bool) {
        let ctx = self.ctx.load(Ordering::Acquire);
        if ctx.is_null() {
            return;
        }
        let mut target = OpenGlFbo {
            fbo,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = flip_y.into();
        let mut params = [
            RenderParam {
                kind: PARAM_OPENGL_FBO,
                data: (&raw mut target).cast(),
            },
            RenderParam {
                kind: PARAM_FLIP_Y,
                data: (&raw mut flip).cast(),
            },
            RenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        unsafe {
            (self.mpv.api.render_context_update)(ctx);
            (self.mpv.api.render_context_render)(ctx, params.as_mut_ptr());
        }
    }

    pub fn report_swap(&self) {
        let ctx = self.ctx.load(Ordering::Acquire);
        if !ctx.is_null() {
            unsafe { (self.mpv.api.render_context_report_swap)(ctx) };
        }
    }

    /// Must run while the GL context is still current; later `render` calls do nothing.
    pub fn destroy(&self) {
        let ctx = self.ctx.swap(ptr::null_mut(), Ordering::AcqRel);
        if !ctx.is_null() {
            unsafe {
                (self.mpv.api.render_context_set_update_callback)(ctx, None, ptr::null_mut());
                (self.mpv.api.render_context_free)(ctx);
            }
        }
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tries_only_full_paths_exe_dir_first() {
        let tools = std::env::temp_dir().join("tools");
        let bin = std::env::temp_dir().join("bin");
        let app = std::env::temp_dir().join("app");
        let path =
            std::env::join_paths([tools.as_path(), Path::new("relative"), bin.as_path()]).unwrap();
        let got = candidates_for("", Some(&app), Some(&path), true);
        assert_eq!(
            got[0],
            app.join("libmpv-2.dll").to_string_lossy(),
            "the exe directory comes first"
        );
        assert!(got.iter().all(|c| Path::new(c).is_absolute()), "{got:?}");
        assert!(got.iter().all(|c| !c.contains("relative")));
        assert!(got.iter().any(|c| Path::new(c).starts_with(&tools)));
        assert!(got.iter().any(|c| Path::new(c).starts_with(&bin)));
    }

    #[test]
    fn configured_path_wins_everywhere() {
        for windows in [true, false] {
            assert_eq!(
                candidates_for("custom/mpv.dll", None, None, windows),
                vec!["custom/mpv.dll"]
            );
        }
    }

    #[test]
    fn unix_keeps_bare_names_for_the_system_loader() {
        let got = candidates_for("", Some(Path::new("/opt/lp")), None, false);
        assert!(got.iter().any(|c| !c.contains(['/', '\x5c'])), "{got:?}");
    }
}
