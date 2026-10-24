//! The row menu: `Row::menu` as a `gio::Menu`, and the "row" action group.

use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gio, glib};
use leafport_app::{MenuAction, Row};

use crate::window::Window;

/// One menu item per entry of `row.menu`, in order, each targeting the row key.
/// "Show in folder" is left out when the row has no folder. A disabled "Cancel sync"
/// names an action the group does not define, so GTK shows it insensitive.
pub fn menu_model(row: &Row) -> gio::Menu {
    let menu = gio::Menu::new();
    for entry in &row.menu {
        let (label, action) = match entry {
            MenuAction::CancelSync { enabled: true } => ("Cancel sync", "row.cancel"),
            MenuAction::CancelSync { enabled: false } => ("Cancel sync", "row.cancel-disabled"),
            MenuAction::SyncAgain => ("Sync again", "row.sync"),
            MenuAction::ReturnLoan => ("Return loan", "row.return"),
            MenuAction::ShowInFolder if row.folder.is_none() => continue,
            MenuAction::ShowInFolder => ("Show in folder", "row.show-in-folder"),
            MenuAction::RemoveFromDisk => ("Remove from disk…", "row.remove"),
        };
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&row.key.to_variant()));
        menu.append_item(&item);
    }
    menu
}

/// The "row" group: open, sync, cancel, return, show-in-folder and remove, each taking
/// the row key (`s`). An action whose row is gone does nothing.
pub fn action_group(window: &Rc<Window>) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, run: fn(&Window, &str)| {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let window: Weak<Window> = Rc::downgrade(window);
        action.connect_activate(move |_, parameter| {
            if let (Some(window), Some(key)) = (
                window.upgrade(),
                parameter.and_then(|value| value.get::<String>()),
            ) {
                run(&window, &key);
            }
        });
        group.add_action(&action);
    };
    add("open", |window, key| {
        if let Some(path) = window.row(key).and_then(|row| row.publication) {
            launch(window, path, false);
        }
    });
    add("sync", |window, key| window.core().sync(key.to_owned()));
    add("cancel", |window, key| window.core().cancel(key.to_owned()));
    add("return", |window, key| {
        window.core().return_loan(key.to_owned())
    });
    add("show-in-folder", |window, key| {
        if let Some(folder) = window.row(key).and_then(|row| row.folder) {
            launch(window, folder, true);
        }
    });
    add("remove", |window, key| window.confirm_remove(key));
    group
}

/// Open `path` in its default application, or show it in the file manager when
/// `containing` is set. A failure is reported through the app's messages.
fn launch(window: &Window, path: String, containing: bool) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
    let core = window.core();
    let done = move |result: Result<(), glib::Error>| {
        if let Err(e) = result
            && !e.matches(gtk::DialogError::Dismissed)
        {
            core.add_message(format!("Could not open {path}: {e}"));
        }
    };
    let parent = window.widget();
    if containing {
        launcher.open_containing_folder(Some(&parent), gio::Cancellable::NONE, done);
    } else {
        launcher.launch(Some(&parent), gio::Cancellable::NONE, done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafport_app::{Activity, RowState};

    fn row(menu: Vec<MenuAction>, folder: Option<&str>) -> Row {
        Row {
            key: "k1".into(),
            title: "t".into(),
            author: None,
            format: "EPUB".into(),
            state: RowState::Synced,
            expires: None,
            cover_path: None,
            folder: folder.map(Into::into),
            publication: None,
            error: None,
            warning: None,
            on_loan: false,
            activity: Activity::Idle,
            primary: None,
            menu,
        }
    }

    fn attribute(menu: &gio::Menu, i: i32, name: &str) -> Option<glib::Variant> {
        menu.item_attribute_value(i, name, None)
    }

    fn strings(menu: &gio::Menu, name: &str) -> Vec<String> {
        (0..menu.n_items())
            .map(|i| attribute(menu, i, name).unwrap().get::<String>().unwrap())
            .collect()
    }

    #[test]
    fn menu_follows_row_menu_order() {
        let menu = menu_model(&row(
            vec![
                MenuAction::SyncAgain,
                MenuAction::ReturnLoan,
                MenuAction::ShowInFolder,
                MenuAction::RemoveFromDisk,
            ],
            Some("/b/t"),
        ));
        assert_eq!(
            strings(&menu, "label"),
            [
                "Sync again",
                "Return loan",
                "Show in folder",
                "Remove from disk…"
            ]
        );
        assert_eq!(
            strings(&menu, "action"),
            ["row.sync", "row.return", "row.show-in-folder", "row.remove"]
        );
    }

    #[test]
    fn menu_targets_row_key() {
        let menu = menu_model(&row(vec![MenuAction::SyncAgain], None));
        assert_eq!(attribute(&menu, 0, "target"), Some("k1".to_variant()));
    }

    #[test]
    fn show_in_folder_needs_folder() {
        let menu = menu_model(&row(vec![MenuAction::ShowInFolder], None));
        assert_eq!(menu.n_items(), 0);
    }

    #[test]
    fn disabled_cancel_uses_undefined_action() {
        let disabled = menu_model(&row(vec![MenuAction::CancelSync { enabled: false }], None));
        assert_eq!(strings(&disabled, "action"), ["row.cancel-disabled"]);
        let enabled = menu_model(&row(vec![MenuAction::CancelSync { enabled: true }], None));
        assert_eq!(strings(&enabled, "action"), ["row.cancel"]);
    }
}
