//! The Settings window: books folder and theme.

use eframe::egui;
use leafport_core::library::{Settings, Theme};

/// Map the stored theme to egui's preference.
pub fn theme_preference(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    }
}

fn theme_label(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "System",
        Theme::Light => "Light",
        Theme::Dark => "Dark",
    }
}

/// The open Settings window and its current edits.
#[derive(Default)]
pub struct SettingsView {
    pub open: bool,
    draft: Settings,
}

impl SettingsView {
    /// Open the window with a copy of the current settings.
    pub fn open(&mut self, current: &Settings) {
        self.draft = current.clone();
        self.open = true;
    }

    /// Draw the window. Returns edited settings as soon as a control changes.
    /// While `busy`, books folder selection is disabled.
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        current: &Settings,
        busy: bool,
    ) -> Option<Settings> {
        let mut open = self.open;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Books folder");
                ui.horizontal(|ui| {
                    let shown = self
                        .draft
                        .books_dir_or_default()
                        .map(|dir| dir.display().to_string())
                        .unwrap_or_else(|| "(none)".into());
                    ui.monospace(shown);
                    if ui
                        .add_enabled(!busy, egui::Button::new("Choose…"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.draft.books_dir = Some(dir);
                    }
                });
                ui.weak("Existing title folders are not moved.");
                if busy {
                    ui.weak("The books folder can change once the running sync finishes.");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text(theme_label(self.draft.theme))
                        .show_ui(ui, |ui| {
                            for theme in [Theme::System, Theme::Light, Theme::Dark] {
                                ui.selectable_value(
                                    &mut self.draft.theme,
                                    theme,
                                    theme_label(theme),
                                );
                            }
                        });
                });
            });
        self.open = open;
        (self.draft != *current).then(|| self.draft.clone())
    }
}
