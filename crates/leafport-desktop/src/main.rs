#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod account;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod app;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod list;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod settings_view;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> eframe::Result {
    app::run()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("On Linux, run leafport-gtk: cargo run -p leafport-gtk");
    std::process::exit(1);
}
