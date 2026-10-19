//! Platform directories for settings, cache, and the default books folder.

use std::ffi::OsString;
use std::path::PathBuf;

const APP: &str = "leafport";

/// Platform data base (before the `leafport` segment).
fn data_base(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env("APPDATA").filter(|v| !v.is_empty()).map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        env("HOME")
            .filter(|v| !v.is_empty())
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        xdg(env, "XDG_DATA_HOME", ".local/share")
    }
}

fn config_base(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        data_base(env)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        xdg(env, "XDG_CONFIG_HOME", ".config")
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn xdg(env: &impl Fn(&str) -> Option<OsString>, var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(v) = env(var).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    env("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(fallback))
}

pub fn data_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    data_base(&env).map(|b| b.join(APP))
}

pub fn config_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    config_base(&env).map(|b| b.join(APP))
}

pub fn data_dir() -> Option<PathBuf> {
    data_dir_from(|k| std::env::var_os(k))
}

pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(|k| std::env::var_os(k))
}

pub fn default_books_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("Books"))
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.yaml"))
}

pub fn cache_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("cloud-cache.xml"))
}
