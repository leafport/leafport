#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(not(target_os = "linux"))]
mod account;
#[cfg(not(target_os = "linux"))]
mod app;
#[cfg(not(target_os = "linux"))]
mod list;
#[cfg(not(target_os = "linux"))]
mod settings_view;

#[cfg(not(target_os = "linux"))]
fn main() -> eframe::Result {
    app::run()
}

#[cfg(target_os = "linux")]
fn main() {
    eprintln!("On Linux, run leafport-gtk: cargo run -p leafport-gtk");
    std::process::exit(1);
}
