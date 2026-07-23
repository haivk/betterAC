//! C ABI over ac-core for the macOS SwiftUI app.
//!
//! The boundary is deliberately coarse: JSON in, JSON out, one string at a time.
//! Marshaling Rust structs across the C ABI by hand is where FFI bugs breed, so
//! instead every call that carries structured data does it as a UTF-8 JSON string
//! and both sides use their normal serializers (serde here, Codable in Swift).
//! The only types crossing the boundary are `char*` and `void`.
//!
//! ## Memory
//!
//! Every function that returns a `char*` returns a heap string the caller now
//! owns; Swift must hand it back to [`ac_string_free`] when done. A returned null
//! pointer means "no value" — for the `*_set`/`launch` calls that is *success*
//! (no error string), for the getters it never happens (they fall back to `"[]"`
//! / `"{}"`). `ac_core_version` is the one exception: it returns a pointer to
//! static memory and must NOT be freed.
//!
//! ## The runtime is platform-selected
//!
//! `make_runtime()` and `platform_launch()` resolve to the Wine runtime on macOS
//! (the real target) and to Proton elsewhere, so this crate still compiles as part
//! of the workspace on Linux. The setup thread and launch path are otherwise
//! identical regardless of which one is underneath — that is the whole point of
//! the shared `Runtime` trait.

use ac_core::config::Config;
use ac_core::install::Install;
use ac_core::servers::Server;
use ac_core::setup::{run_all, Progress, RunState, Runtime};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::{Mutex, OnceLock};

// --- platform selection -----------------------------------------------------

#[cfg(target_os = "macos")]
type PlatformRuntime = ac_core::wine::WineRuntime;
#[cfg(not(target_os = "macos"))]
type PlatformRuntime = ac_core::proton::ProtonRuntime;

/// The runtime for this platform, pointed at the configured prefix.
fn make_runtime() -> PlatformRuntime {
    PlatformRuntime::new(Config::load().prefix)
}

/// Launch the client through whichever runtime this platform uses. `res` is left
/// `None` here; the frontend can pass a detected resolution in a later revision.
fn platform_launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
) -> Result<std::process::Child, String> {
    #[cfg(target_os = "macos")]
    {
        ac_core::wine::launch(install, server, account, password, None)
    }
    #[cfg(not(target_os = "macos"))]
    {
        ac_core::proton::launch(install, server, account, password, None)
    }
}

// --- C string helpers -------------------------------------------------------

/// Move a Rust string onto the heap as a C string the caller owns.
fn to_c(s: impl Into<Vec<u8>>) -> *mut c_char {
    // An interior NUL is the only failure; a JSON/utf-8 payload never has one, but
    // fall back to an empty string rather than panic across the FFI boundary.
    CString::new(s).unwrap_or_default().into_raw()
}

/// null on success, the error text otherwise — the convention for the calls whose
/// only interesting output is whether they failed.
fn err_to_c(r: Result<(), String>) -> *mut c_char {
    match r {
        Ok(()) => ptr::null_mut(),
        Err(e) => to_c(e),
    }
}

/// Borrow a C string as `&str`, or `None` if it is null or not valid UTF-8.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated C string that outlives the borrow.
unsafe fn from_c<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

// --- setup progress state ---------------------------------------------------

/// The running-setup state the background thread folds progress into and
/// `ac_setup_poll` reads. One process runs setup at most once, so one global is
/// enough. It exists before setup starts, holding every step as pending — that
/// is what lets the app show the plan up front.
fn setup_state() -> &'static Mutex<RunState> {
    static S: OnceLock<Mutex<RunState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(RunState::new()))
}

// --- exported ABI -----------------------------------------------------------

/// The ac-core semver the app was linked against. Points at static memory — do
/// NOT pass this one to `ac_string_free`.
#[no_mangle]
pub extern "C" fn ac_core_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const _
}

/// The server directory as a JSON array. Live from treestats when reachable,
/// otherwise the compiled-in snapshot — this never fails, so it never returns
/// null. Free with `ac_string_free`.
#[no_mangle]
pub extern "C" fn ac_servers_json() -> *mut c_char {
    let list = ac_core::servers::fetch().unwrap_or_else(|_| ac_core::servers::bundled());
    to_c(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))
}

/// The persisted config (prefix, saved servers, last selection) as JSON. Free
/// with `ac_string_free`.
#[no_mangle]
pub extern "C" fn ac_config_get() -> *mut c_char {
    to_c(serde_json::to_string(&Config::load()).unwrap_or_else(|_| "{}".into()))
}

/// Replace the persisted config from a JSON object. Returns null on success, or
/// an error string (free it with `ac_string_free`).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn ac_config_set(json: *const c_char) -> *mut c_char {
    let Some(s) = from_c(json) else {
        return to_c("ac_config_set: null or non-UTF-8 config");
    };
    match serde_json::from_str::<Config>(s) {
        Ok(cfg) => err_to_c(cfg.save()),
        Err(e) => to_c(format!("ac_config_set: invalid config JSON: {e}")),
    }
}

/// Whether the game is set up, as JSON `{"ready":bool,"ac_dir":string|null,
/// "error":string|null}`. `ready:false` with an `error` explaining what is
/// missing is the signal for the app to show the setup screen. Free with
/// `ac_string_free`.
#[no_mangle]
pub extern "C" fn ac_detect() -> *mut c_char {
    let v = match make_runtime().discover() {
        Ok(install) => serde_json::json!({
            "ready": true,
            "ac_dir": install.ac_dir.to_string_lossy(),
            "error": serde_json::Value::Null,
        }),
        Err(e) => serde_json::json!({
            "ready": false,
            "ac_dir": serde_json::Value::Null,
            "error": e,
        }),
    };
    to_c(v.to_string())
}

/// Start first-run setup on a background thread. Calling it while setup is
/// already running does nothing; calling it after a run *finished* starts a fresh
/// one, which is what "Try again" needs — the steps are idempotent, so a retry
/// skips everything that already succeeded and resumes at the one that broke.
/// Poll `ac_setup_poll` for progress.
#[no_mangle]
pub extern "C" fn ac_setup_start() {
    {
        let mut g = setup_state().lock().unwrap();
        if g.started && !g.done {
            return;
        }
        *g = RunState::new();
        g.started = true;
    }
    ac_core::setup::clear_cancel();

    std::thread::spawn(|| {
        let rt = make_runtime();
        let result = run_all(&rt, &mut |p: Progress| {
            setup_state().lock().unwrap().apply(&p);
        });
        setup_state().lock().unwrap().finish(&result);
    });
}

/// Ask a running setup to stop. Returns immediately; the run ends at the next
/// cancellation point — at once during a download, otherwise when the step's
/// external command (wineboot, the installer wizard) returns. Watch
/// `ac_setup_poll` for `done:true` with `cancelled:true`. Steps are idempotent, so
/// `ac_setup_start` afterwards resumes rather than starting over.
#[no_mangle]
pub extern "C" fn ac_setup_cancel() {
    ac_core::setup::request_cancel();
}

/// A snapshot of the whole setup run as JSON:
///
/// ```json
/// {"started":bool, "done":bool, "cancelled":bool, "error":string|null,
///  "steps":[{"step":"download_client", "label":"Downloading the game client",
///            "detail":"The original retail installer, about 570 MB",
///            "state":"pending|running|done|skipped|failed",
///            "fraction":0.0..1.0, "message":"312 MB of 571 MB · 8.4 MB/s"}, …]}
/// ```
///
/// Every step is present from the first call, so polling *before*
/// `ac_setup_start` returns the plan (all `pending`) and the app can show the
/// list before the user commits to it. When `done` is true setup has finished —
/// successfully if `error` is null, otherwise `error` says why it stopped and the
/// step that broke is the one in state `failed`. Free with `ac_string_free`.
#[no_mangle]
pub extern "C" fn ac_setup_poll() -> *mut c_char {
    let g = setup_state().lock().unwrap();
    to_c(serde_json::to_string(&*g).unwrap_or_else(|_| "{}".into()))
}

/// Launch the client for a server (a JSON `Server` object) and account. Returns
/// null on a successful spawn, or an error string (free it with
/// `ac_string_free`). The game runs detached; this returns as soon as it starts.
///
/// # Safety
/// All three pointers must be null or valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn ac_launch(
    server_json: *const c_char,
    account: *const c_char,
    password: *const c_char,
) -> *mut c_char {
    let (Some(sj), Some(acct), Some(pw)) =
        (from_c(server_json), from_c(account), from_c(password))
    else {
        return to_c("ac_launch: null or non-UTF-8 argument");
    };
    let server: Server = match serde_json::from_str(sj) {
        Ok(s) => s,
        Err(e) => return to_c(format!("ac_launch: invalid server JSON: {e}")),
    };
    let install = match make_runtime().discover() {
        Ok(i) => i,
        Err(e) => return to_c(e),
    };
    match platform_launch(&install, &server, acct, pw) {
        // Detach: the child keeps running after we drop the handle.
        Ok(_child) => ptr::null_mut(),
        Err(e) => to_c(e),
    }
}

/// What a reset would delete, as a JSON array of `{"label", "path"}`.
///
/// The UI lists these before asking the user to confirm, so the warning names the
/// actual directories on this machine rather than a hardcoded description that
/// could drift from what `ac_reset` really removes.
#[no_mangle]
pub extern "C" fn ac_reset_targets_json() -> *mut c_char {
    let v: Vec<serde_json::Value> = ac_core::reset::targets()
        .into_iter()
        .map(|t| serde_json::json!({ "label": t.label, "path": t.path.to_string_lossy() }))
        .collect();
    to_c(serde_json::Value::Array(v).to_string())
}

/// Delete the install: prefix, engine, and settings. Returns null on success, or
/// an error string. After this, `ac_detect` reports not-ready and the frontend
/// routes back to setup.
///
/// Refused while a setup run is in flight — deleting the prefix out from under
/// the thread building it would leave a mess neither side could describe.
#[no_mangle]
pub extern "C" fn ac_reset() -> *mut c_char {
    {
        let g = setup_state().lock().unwrap();
        if g.started && !g.done {
            return to_c("Setup is still running. Stop it before resetting.");
        }
    }
    match ac_core::reset::reset() {
        Ok(_) => {
            // A fresh plan, not the finished state of the run that built the
            // prefix we just deleted -- otherwise the setup screen opens showing
            // every step already done.
            *setup_state().lock().unwrap() = RunState::new();
            ptr::null_mut()
        }
        Err(e) => to_c(e),
    }
}

/// Free a string returned by any of the `*_json` / `*_get` / `ac_detect` /
/// `ac_setup_poll` / `ac_config_set` / `ac_launch` calls. Null is ignored. Never
/// call this on `ac_core_version`'s result.
///
/// # Safety
/// `p` must be null or a pointer previously returned by one of this library's
/// string-returning functions, and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn ac_string_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}
