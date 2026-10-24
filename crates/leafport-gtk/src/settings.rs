//! Preferences: the books folder and the theme.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use leafport_app::{AppError, Settings, Status, Theme};

use crate::color_scheme;
use crate::window::Window;

/// The chosen folder as settings text, or the message shown under the folder row.
/// Settings carry the folder as text; a lossy copy would name another folder.
pub fn folder_choice(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "{} cannot be used: its name is not valid Unicode.",
            path.display()
        )
    })
}

const THEMES: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

fn theme_index(theme: &Theme) -> u32 {
    THEMES.iter().position(|t| t == theme).unwrap_or(0) as u32
}

/// The preferences dialog and the widgets that follow the settings and the status.
pub struct PreferencesDialog {
    dialog: adw::PreferencesDialog,
    group: adw::PreferencesGroup,
    folder: adw::ActionRow,
    choose: gtk::Button,
    error: adw::ActionRow,
    theme: adw::ComboRow,
    /// Set while `update` changes widgets, so their handlers stay quiet.
    updating: Cell<bool>,
}

/// Show the preferences over `window`.
pub fn present(window: &Rc<Window>) {
    if let Some(dialog) = window.preferences() {
        dialog.dialog.present(Some(&window.widget()));
        return;
    }
    let dialog = PreferencesDialog::build();
    dialog.connect(window);
    dialog.update(&window.settings(), &window.status());
    window.set_preferences(Some(dialog.clone()));
    dialog.dialog.present(Some(&window.widget()));
}

impl PreferencesDialog {
    /// Follow the settings in effect and the app status.
    pub fn update(&self, settings: &Settings, status: &Status) {
        self.updating.set(true);
        let shown = settings
            .books_dir
            .as_deref()
            .or(status.books_dir.as_deref())
            .unwrap_or("(none)");
        self.folder.set_subtitle(shown);
        self.choose.set_sensitive(!status.job_running);
        let mut description = String::from("Existing title folders are not moved.");
        if status.job_running {
            description.push_str("\nThe books folder can change once the running sync finishes.");
        }
        self.group.set_description(Some(&description));
        self.theme.set_selected(theme_index(&settings.theme));
        self.updating.set(false);
    }

    fn show_error(&self, message: Option<&str>) {
        self.error.set_visible(message.is_some());
        self.error.set_title(message.unwrap_or(""));
    }

    fn build() -> Rc<Self> {
        let folder = adw::ActionRow::builder()
            .title("Books folder")
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        let choose = gtk::Button::builder()
            .label("Choose\u{2026}")
            .valign(gtk::Align::Center)
            .build();
        folder.add_suffix(&choose);
        let error = adw::ActionRow::builder()
            .use_markup(false)
            .title_selectable(true)
            .visible(false)
            .build();
        error.add_css_class("error");

        let names = gtk::StringList::new(&["System", "Light", "Dark"]);
        let theme = adw::ComboRow::builder()
            .title("Theme")
            .model(&names)
            .build();

        let group = adw::PreferencesGroup::new();
        group.add(&folder);
        group.add(&error);
        group.add(&theme);
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Preferences");
        dialog.add(&page);
        Rc::new(Self {
            dialog,
            group,
            folder,
            choose,
            error,
            theme,
            updating: Cell::new(false),
        })
    }

    fn connect(self: &Rc<Self>, window: &Rc<Window>) {
        let this = Rc::downgrade(self);
        let win = Rc::downgrade(window);
        self.choose.connect_clicked(glib::clone!(
            #[strong]
            win,
            #[strong]
            this,
            move |button| {
                let (Some(win), Some(this)) = (win.upgrade(), this.upgrade()) else {
                    return;
                };
                let parent = button.root().and_downcast::<gtk::Window>();
                gtk::FileDialog::new().select_folder(
                    parent.as_ref(),
                    None::<&gio::Cancellable>,
                    move |result| {
                        // A dismissed dialog is an error value too; nothing to do then.
                        let Ok(file) = result else {
                            return;
                        };
                        let choice = match file.path() {
                            Some(path) => folder_choice(&path),
                            None => Err(format!(
                                "{} cannot be used: it is not a local folder.",
                                file.uri()
                            )),
                        };
                        match choice {
                            Ok(dir) => {
                                this.show_error(None);
                                let mut new = win.settings();
                                if new.books_dir.as_deref() != Some(dir.as_str()) {
                                    new.books_dir = Some(dir);
                                    commit(&win, new);
                                }
                            }
                            Err(message) => this.show_error(Some(&message)),
                        }
                    },
                );
            }
        ));
        self.theme.connect_selected_notify(glib::clone!(
            #[strong]
            win,
            #[strong]
            this,
            move |row| {
                let (Some(win), Some(this)) = (win.upgrade(), this.upgrade()) else {
                    return;
                };
                if this.updating.get() {
                    return;
                }
                let Some(theme) = THEMES.get(row.selected() as usize) else {
                    return;
                };
                let mut new = win.settings();
                if new.theme != *theme {
                    new.theme = theme.clone();
                    commit(&win, new);
                }
            }
        ));
        self.dialog.connect_closed(glib::clone!(
            #[strong]
            win,
            move |_| {
                if let Some(win) = win.upgrade() {
                    win.set_preferences(None);
                }
            }
        ));
    }
}

/// Save `new` at once. A failed save is already in the banner and the settings still
/// apply; a busy app keeps the settings in effect and says so.
fn commit(window: &Rc<Window>, new: Settings) {
    match window.core().set_settings(new.clone()) {
        Ok(()) | Err(AppError::Save { .. }) => {
            adw::StyleManager::default().set_color_scheme(color_scheme(&new.theme));
            window.set_settings(new);
        }
        Err(AppError::Busy) => {
            window
                .core()
                .add_message("The books folder can change once the running job finishes.".into());
        }
    }
    // On busy this resets the rows to the settings in effect.
    window.update_preferences();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_choice_accepts_utf8() {
        assert_eq!(
            folder_choice(Path::new("/home/u/Books")),
            Ok("/home/u/Books".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn folder_choice_rejects_non_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        let error = folder_choice(path).unwrap_err();
        assert!(error.ends_with("cannot be used: its name is not valid Unicode."));
    }
}
