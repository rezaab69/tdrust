//! Access to TDLib's JSON interface (`td_json_client.h`).

use std::ffi::{c_char, c_double, c_int, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::{Error, Result};

/// The four functions of TDLib's JSON interface.
///
/// [`Tdjson`] implements this trait on top of the real library. A custom
/// implementation can stand in for TDLib, which is how tdrust's own tests
/// exercise the runtime without a Telegram connection.
pub trait Backend: Send + Sync + 'static {
    /// Reserves a new client identifier (`td_create_client_id`). The client
    /// comes to life when the first request is sent to it.
    fn create_client_id(&self) -> i32;

    /// Sends a JSON-serialized request to a client (`td_send`).
    fn send(&self, client_id: i32, request: &str);

    /// Waits up to `timeout` for the next response or update of any client
    /// (`td_receive`). Only one thread calls this at a time.
    fn receive(&self, timeout: Duration) -> Option<String>;

    /// Executes a request synchronously (`td_execute`). Only requests
    /// documented as "Can be called synchronously" are supported.
    fn execute(&self, request: &str) -> Option<String>;
}

type CreateClientIdFn = unsafe extern "C" fn() -> c_int;
type SendFn = unsafe extern "C" fn(c_int, *const c_char);
type ReceiveFn = unsafe extern "C" fn(c_double) -> *const c_char;
type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *const c_char;

/// The `tdjson` shared library.
pub struct Tdjson {
    create_client_id: CreateClientIdFn,
    send: SendFn,
    receive: ReceiveFn,
    execute: ExecuteFn,
    /// `td_receive` returns a buffer that is only valid until the next call,
    /// so calls are serialized.
    receive_lock: Mutex<()>,
    /// Keeps the library mapped for as long as the function pointers above
    /// are in use. `None` when linked at build time.
    _library: Option<libloading::Library>,
}

impl std::fmt::Debug for Tdjson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tdjson").field("dynamic", &self._library.is_some()).finish_non_exhaustive()
    }
}

impl Tdjson {
    /// Loads `tdjson` from the usual places:
    ///
    /// 1. the file named by the `TDJSON_PATH` environment variable;
    /// 2. the directory of the running executable;
    /// 3. the system library search path (`PATH` on Windows,
    ///    `LD_LIBRARY_PATH`/`ldconfig` on Linux, `DYLD_LIBRARY_PATH` on macOS).
    ///
    /// With the `link` feature enabled, returns the library linked at build time.
    pub fn load() -> Result<Self> {
        #[cfg(feature = "link")]
        {
            Ok(Self::linked())
        }
        #[cfg(not(feature = "link"))]
        {
            Self::load_dynamic()
        }
    }

    #[cfg_attr(feature = "link", allow(dead_code))]
    fn load_dynamic() -> Result<Self> {
        if let Some(path) = std::env::var_os("TDJSON_PATH") {
            return Self::load_from(path);
        }

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = std::env::current_exe().ok().as_deref().and_then(Path::parent) {
            candidates.extend(library_names().iter().map(|name| dir.join(name)));
        }
        candidates.extend(library_names().iter().map(PathBuf::from));

        let mut errors = Vec::new();
        for candidate in &candidates {
            match Self::load_from(candidate) {
                Ok(library) => return Ok(library),
                Err(err) => errors.push(err.to_string()),
            }
        }
        Err(Error::Library(format!(
            "tdjson was not found (tried {}). Build TDLib (https://tdlib.github.io/td/build.html) and either \
             place the library next to your executable, add its directory to the library search path, or set \
             TDJSON_PATH to the library file. Details: {}",
            candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", "),
            errors.last().map(String::as_str).unwrap_or("none"),
        )))
    }

    /// Loads `tdjson` from a specific file.
    pub fn load_from(path: impl AsRef<std::ffi::OsStr>) -> Result<Self> {
        let path = path.as_ref();
        let describe = |err: libloading::Error| Error::Library(format!("{}: {err}", Path::new(path).display()));

        // SAFETY: loading a library runs its initializers. tdjson's
        // initializers have no preconditions.
        let library = unsafe { libloading::Library::new(path) }.map_err(describe)?;
        // SAFETY: the symbols have exactly these signatures in
        // td/telegram/td_json_client.h, and the pointers stay valid because
        // the library is stored alongside them.
        unsafe {
            let create_client_id = *library.get::<CreateClientIdFn>(b"td_create_client_id\0").map_err(describe)?;
            let send = *library.get::<SendFn>(b"td_send\0").map_err(describe)?;
            let receive = *library.get::<ReceiveFn>(b"td_receive\0").map_err(describe)?;
            let execute = *library.get::<ExecuteFn>(b"td_execute\0").map_err(describe)?;
            Ok(Tdjson {
                create_client_id,
                send,
                receive,
                execute,
                receive_lock: Mutex::new(()),
                _library: Some(library),
            })
        }
    }

    /// The library linked at build time (requires the `link` feature).
    #[cfg(feature = "link")]
    pub fn linked() -> Self {
        Tdjson {
            create_client_id: linked::td_create_client_id,
            send: linked::td_send,
            receive: linked::td_receive,
            execute: linked::td_execute,
            receive_lock: Mutex::new(()),
            _library: None,
        }
    }
}

#[cfg(feature = "link")]
mod linked {
    use std::ffi::{c_char, c_double, c_int};

    #[link(name = "tdjson")]
    extern "C" {
        pub fn td_create_client_id() -> c_int;
        pub fn td_send(client_id: c_int, request: *const c_char);
        pub fn td_receive(timeout: c_double) -> *const c_char;
        pub fn td_execute(request: *const c_char) -> *const c_char;
    }
}

/// File names under which TDLib's build installs the JSON interface.
fn library_names() -> Vec<String> {
    if cfg!(target_os = "windows") {
        vec!["tdjson.dll".into()]
    } else if cfg!(target_os = "macos") {
        vec!["libtdjson.dylib".into(), format!("libtdjson.{}.dylib", crate::BUNDLED_TDLIB_VERSION)]
    } else {
        vec!["libtdjson.so".into(), format!("libtdjson.so.{}", crate::BUNDLED_TDLIB_VERSION)]
    }
}

/// Copies a string returned by TDLib before the buffer is reused.
///
/// # Safety
///
/// `ptr` must be null or point to a NUL-terminated string.
unsafe fn copy_response(ptr: *const c_char) -> Option<String> {
    (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

fn to_c_string(request: &str) -> CString {
    // Requests are produced by serde_json, which escapes NUL characters.
    CString::new(request).expect("TDLib requests never contain NUL bytes")
}

impl Backend for Tdjson {
    fn create_client_id(&self) -> i32 {
        // SAFETY: takes no arguments and is thread-safe.
        unsafe { (self.create_client_id)() }
    }

    fn send(&self, client_id: i32, request: &str) {
        let request = to_c_string(request);
        // SAFETY: `request` is a valid C string for the duration of the call;
        // td_send copies it and is thread-safe.
        unsafe { (self.send)(client_id, request.as_ptr()) }
    }

    fn receive(&self, timeout: Duration) -> Option<String> {
        let _guard = self.receive_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: calls are serialized by `receive_lock`, and the returned
        // buffer is copied before the lock is released.
        unsafe { copy_response((self.receive)(timeout.as_secs_f64())) }
    }

    fn execute(&self, request: &str) -> Option<String> {
        let request = to_c_string(request);
        // SAFETY: the returned buffer is thread-local and valid until the next
        // td_execute call on this thread; it is copied immediately.
        unsafe { copy_response((self.execute)(request.as_ptr())) }
    }
}
