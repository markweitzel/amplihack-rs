//! Shared test environment helpers for modules that mutate process-global
//! state (`HOME`, current directory). Tests must serialize on
//! [`home_env_lock`] before mutating these globals to avoid cross-test races.
#![allow(dead_code)]

use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Single global lock for all environment-mutating tests in this crate.
pub fn home_env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

/// Set `HOME` to `path`, returning the previous value for later restoration.
pub fn set_home(path: &Path) -> Option<std::ffi::OsString> {
    let previous = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", path);
    }
    previous
}

/// Restore (or unset) `HOME` from a value produced by [`set_home`].
pub fn restore_home(previous: Option<std::ffi::OsString>) {
    if let Some(value) = previous {
        unsafe { std::env::set_var("HOME", value) };
    } else {
        unsafe { std::env::remove_var("HOME") };
    }
}

/// Set the process current directory to `path`, returning the previous
/// directory for later restoration via [`restore_cwd`].
pub fn set_cwd(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let previous = std::env::current_dir()?;
    std::env::set_current_dir(path)?;
    Ok(previous)
}

/// Restore the process current directory to `previous`.
pub fn restore_cwd(previous: &Path) -> std::io::Result<()> {
    std::env::set_current_dir(previous)
}
