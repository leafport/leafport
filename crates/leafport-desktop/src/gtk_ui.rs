//! The Linux front end: GTK 4 and libadwaita over `leafport-app`.

mod account;
mod covers;
mod operations;
mod row_menu;
mod row_widget;
mod settings;
mod store;
mod window;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use leafport_app::{App, Event, Listener};

use crate::gtk_ui::window::Window;

const APP_ID: &str = "dev.leafport.Leafport";

/// Hands app events from the reactor thread to the main loop.
struct Forward {
    tx: async_channel::Sender<Event>,
}

impl Listener for Forward {
    fn on_event(&self, event: Event) {
        // The receiver is gone once the main loop quit; the event is not needed then.
        let _ = self.tx.try_send(event);
    }
}

/// The paths of `files` whose extension is "acsm", case-insensitive, as strings.
/// Files without a local path, or whose path is not valid UTF-8, are skipped.
fn acsm_paths(files: &[gio::File]) -> Vec<String> {
    files
        .iter()
        .filter_map(|file| file.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("acsm"))
        })
        .filter_map(|path| path.into_os_string().into_string().ok())
        .collect()
}

/// The app core and its window, created on the first `activate` or `open`.
struct Shell {
    core: Arc<App>,
    window: Rc<Window>,
}

/// Return the shell, creating the core, the window and the event loop on first use.
fn shell(app: &adw::Application, slot: &RefCell<Option<Rc<Shell>>>) -> Rc<Shell> {
    if let Some(shell) = slot.borrow().as_ref() {
        return shell.clone();
    }
    let (tx, rx) = async_channel::unbounded();
    let core = App::new(Box::new(Forward { tx }));
    let snapshot = core.snapshot();
    let window = Window::new(app, core.clone(), snapshot);
    glib::spawn_future_local(glib::clone!(
        #[strong]
        window,
        async move {
            while let Ok(event) = rx.recv().await {
                window.apply(event);
            }
        }
    ));
    let shell = Rc::new(Shell { core, window });
    *slot.borrow_mut() = Some(shell.clone());
    shell
}

/// Run the GTK app until its last window closes.
pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let slot: Rc<RefCell<Option<Rc<Shell>>>> = Rc::default();
    app.connect_startup(glib::clone!(
        #[strong]
        slot,
        move |app| {
            // The action only exists once a window does; the menu is in the window.
            let preferences = gio::ActionEntry::builder("preferences")
                .activate(glib::clone!(
                    #[strong]
                    slot,
                    move |_: &adw::Application, _, _| {
                        let shell = slot.borrow().clone();
                        if let Some(shell) = shell {
                            settings::present(&shell.window);
                        }
                    }
                ))
                .build();
            app.add_action_entries([preferences]);
            app.set_accels_for_action("app.preferences", &["<Control>comma"]);
            app.set_accels_for_action("win.import", &["<Control>o"]);
            app.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);
            app.set_accels_for_action("win.show-sidebar", &["F9"]);
            row_widget::load_css();
        }
    ));
    app.connect_activate(glib::clone!(
        #[strong]
        slot,
        move |app| shell(app, &slot).window.present()
    ));
    app.connect_open(move |app, files, _hint| {
        let shell = shell(app, &slot);
        for path in acsm_paths(files) {
            shell.core.import_acsm(path);
        }
        shell.window.present();
    });
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acsm_paths_keeps_only_acsm() {
        let dir = std::env::temp_dir();
        let files = [
            gio::File::for_path(dir.join("a.acsm")),
            gio::File::for_path(dir.join("B.ACSM")),
            gio::File::for_path(dir.join("book.epub")),
            gio::File::for_path(dir.join("dir/")),
        ];
        assert_eq!(
            acsm_paths(&files),
            vec![
                dir.join("a.acsm").to_str().unwrap().to_owned(),
                dir.join("B.ACSM").to_str().unwrap().to_owned(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn acsm_paths_skips_non_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = std::env::temp_dir().join(OsStr::from_bytes(b"\xff.acsm"));
        assert!(acsm_paths(&[gio::File::for_path(path)]).is_empty());
    }

    #[test]
    fn forward_after_receiver_dropped_does_not_panic() {
        let (tx, rx) = async_channel::unbounded();
        drop(rx);
        let forward = Forward { tx };
        forward.on_event(Event::RowRemoved { key: "a".into() });
    }
}
