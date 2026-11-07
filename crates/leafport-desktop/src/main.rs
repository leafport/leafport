#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
//! Leafport's desktop app: GTK and libadwaita on Linux and the other Unix desktops,
//! egui on macOS and Windows.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod egui_ui;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod gtk_ui;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> eframe::Result {
    egui_ui::app::run()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() -> gtk::glib::ExitCode {
    gtk_ui::run()
}
