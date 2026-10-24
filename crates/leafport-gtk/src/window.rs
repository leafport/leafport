//! The main window: split view with the account sidebar, header bar, banner and list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use leafport_app::{App, Event, Row, Snapshot, Status};

use crate::covers::Covers;
use crate::row_menu;
use crate::row_widget::RowWidget;
use crate::store::{RowObject, RowStore};

const SIGNED_OUT_HINT: &str = "Sign in to see the books in your account.";

/// The application window and the widgets that follow the app state.
pub struct Window {
    window: adw::ApplicationWindow,
    core: Arc<App>,
    store: RowStore,
    covers: Rc<Covers>,
    /// Shows `signed-in` or `signed-out` in the sidebar.
    account: gtk::Stack,
    user: gtk::Label,
    refresh: gtk::Button,
    refresh_spinner: gtk::Spinner,
    /// The account note below the refresh button. Task 4 fills it.
    note: gtk::Label,
    importing: gtk::Box,
    importing_spinner: gtk::Spinner,
    sync_all: gtk::Button,
    cancel_all: gtk::Button,
    banner: adw::Banner,
    /// Shows `signed-out` or `list`.
    body: gtk::Stack,
    /// The sign-in hint above the list while signed out.
    list_hint: gtk::Box,
    signed_out: Cell<bool>,
}

impl Window {
    /// Build the window for `app`, showing `snapshot`.
    pub fn new(app: &adw::Application, core: Arc<App>, snapshot: Snapshot) -> Rc<Window> {
        let store = RowStore::new();
        let covers = Covers::new();

        // Sidebar.
        let user = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let sign_out = gtk::Button::with_label("Sign out");
        let signed_in = vbox(6);
        signed_in.append(
            &gtk::Label::builder()
                .label("Signed in as")
                .xalign(0.0)
                .build(),
        );
        signed_in.append(&user);
        signed_in.append(&sign_out);

        let signed_out = vbox(6);
        signed_out.append(
            &gtk::Label::builder()
                .label("Not signed in")
                .xalign(0.0)
                .build(),
        );
        signed_out.append(&sign_in_button());

        let account = gtk::Stack::new();
        account.add_named(&signed_in, Some("signed-in"));
        account.add_named(&signed_out, Some("signed-out"));

        let refresh = gtk::Button::with_label("Refresh list");
        let refresh_spinner = gtk::Spinner::new();
        let refresh_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        refresh_row.append(&refresh);
        refresh_row.append(&refresh_spinner);

        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .visible(false)
            .build();

        let sidebar_box = vbox(12);
        sidebar_box.set_margin_top(12);
        sidebar_box.set_margin_bottom(12);
        sidebar_box.set_margin_start(12);
        sidebar_box.set_margin_end(12);
        sidebar_box.append(&account);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&refresh_row);
        sidebar_box.append(&note);

        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&adw::HeaderBar::new());
        sidebar_view.set_content(Some(&sidebar_box));
        let sidebar = adw::NavigationPage::new(&sidebar_view, "Leafport");

        // Header bar.
        let import = gtk::Button::with_label("Import ACSM…");
        let importing_spinner = gtk::Spinner::new();
        let importing = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        importing.append(&importing_spinner);
        importing.append(
            &gtk::Label::builder()
                .label("Importing…")
                .css_classes(["dim-label"])
                .build(),
        );

        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("app.preferences"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        let sync_all = gtk::Button::with_label("Sync all");
        let cancel_all = gtk::Button::with_label("Cancel all");

        let header = adw::HeaderBar::new();
        header.pack_start(&import);
        header.pack_start(&importing);
        header.pack_end(&menu_button);
        header.pack_end(&sync_all);
        header.pack_end(&cancel_all);

        let banner = adw::Banner::new("");
        banner.set_use_markup(false);
        banner.set_button_label(Some("Dismiss"));

        // Body.
        let status_page = adw::StatusPage::builder().title(SIGNED_OUT_HINT).build();
        let status_sign_in = sign_in_button();
        status_sign_in.set_halign(gtk::Align::Center);
        status_sign_in.add_css_class("pill");
        status_page.set_child(Some(&status_sign_in));

        let list_hint = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        list_hint.set_margin_top(6);
        list_hint.set_margin_bottom(6);
        list_hint.set_margin_start(12);
        list_hint.set_margin_end(12);
        list_hint.append(&gtk::Label::new(Some(SIGNED_OUT_HINT)));
        list_hint.append(&sign_in_button());

        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(store.model()))),
            Some(row_factory(&covers)),
        );
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let list_page = vbox(0);
        list_page.append(&list_hint);
        list_page.append(&scrolled);

        let body = gtk::Stack::new();
        body.add_named(&status_page, Some("signed-out"));
        body.add_named(&list_page, Some("list"));

        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&header);
        content_view.add_top_bar(&banner);
        content_view.set_content(Some(&body));
        let content = adw::NavigationPage::new(&content_view, "Books");

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&content)
            .show_content(true)
            .build();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Leafport")
            .default_width(1120)
            .default_height(780)
            .width_request(720)
            .height_request(480)
            .content(&split)
            .build();
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 560sp").expect("valid breakpoint"),
        );
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        // Handlers. "Sign in" stays unconnected until Task 4.
        sign_out.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.sign_out()
        ));
        refresh.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.refresh()
        ));
        sync_all.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.sync_all()
        ));
        cancel_all.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.cancel_all()
        ));
        banner.connect_button_clicked(glib::clone!(
            #[strong]
            core,
            move |banner| {
                // Hide at once; the app confirms with a status update.
                banner.set_revealed(false);
                core.clear_banner();
            }
        ));
        import.connect_clicked(glib::clone!(
            #[weak]
            window,
            #[strong]
            core,
            move |_| choose_acsm(&window, core.clone())
        ));

        let this = Rc::new(Window {
            window,
            core,
            store,
            covers,
            account,
            user,
            refresh,
            refresh_spinner,
            note,
            importing,
            importing_spinner,
            sync_all,
            cancel_all,
            banner,
            body,
            list_hint,
            signed_out: Cell::new(true),
        });
        this.window
            .insert_action_group("row", Some(&row_menu::action_group(&this)));
        this.store.set_rows(snapshot.rows);
        this.apply_status(&snapshot.status);
        this
    }

    /// Show the window and bring it to the front.
    pub fn present(&self) {
        self.window.present();
    }

    /// Apply one event from the app.
    pub fn apply(&self, event: Event) {
        let stale = match event {
            Event::Rows { rows } => self.store.set_rows(rows),
            Event::Row { row } => self.store.set_row(row),
            Event::RowRemoved { key } => self.store.remove(&key),
            Event::Progress { key, fraction } => {
                self.store.progress(&key, fraction);
                return;
            }
            // `apply_status` updates the body itself.
            Event::Status { status } => {
                self.apply_status(&status);
                return;
            }
            // The sign-in dialog consumes providers in Task 4.
            Event::Providers { .. } => return,
        };
        self.covers.forget(&stale);
        self.update_body();
    }

    /// The window widget.
    pub fn widget(&self) -> adw::ApplicationWindow {
        self.window.clone()
    }

    /// The app core.
    pub fn core(&self) -> Arc<App> {
        self.core.clone()
    }

    /// The current row of `key`, if listed.
    pub fn row(&self, key: &str) -> Option<Row> {
        self.store.find(key).map(|object| object.row())
    }

    /// Ask before removing the title folder of `key`. Does nothing when the row is gone
    /// or has no folder.
    pub fn confirm_remove(self: &Rc<Self>, key: &str) {
        let Some(row) = self.row(key) else {
            return;
        };
        let Some(folder) = &row.folder else {
            return;
        };
        let mut body = format!("This deletes {folder}.\nThe book stays in your cloud library.");
        if row.on_loan {
            body.push_str(
                "\nThe loan is returned first. The folder is deleted once the return succeeds.",
            );
        }
        let confirm = if row.on_loan {
            "Return and remove"
        } else {
            "Remove"
        };
        let dialog = adw::AlertDialog::new(
            Some(&format!("Remove \"{}\" from disk?", row.title)),
            Some(&body),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", confirm)]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        // The row may change while the dialog is open: read it again on confirm.
        let window = Rc::downgrade(self);
        let key = row.key.clone();
        dialog.connect_response(Some("remove"), move |_, _| {
            let Some(window) = window.upgrade() else {
                return;
            };
            if let Some((key, on_loan)) = remove_args(window.row(&key).as_ref()) {
                window.core.remove(key, on_loan);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn apply_status(&self, status: &Status) {
        match &status.user {
            Some(user) => {
                self.user.set_label(user);
                self.account.set_visible_child_name("signed-in");
            }
            None => self.account.set_visible_child_name("signed-out"),
        }
        self.refresh
            .set_sensitive(status.user.is_some() && !status.refreshing);
        set_spinning(&self.refresh_spinner, status.refreshing);
        self.note.set_visible(!self.note.label().is_empty());

        self.importing.set_visible(status.importing);
        set_spinning(&self.importing_spinner, status.importing);
        self.sync_all.set_sensitive(status.can_sync_all);
        self.cancel_all.set_visible(status.can_cancel_all);

        self.banner.set_title(&status.banner.join("\n"));
        self.banner.set_revealed(!status.banner.is_empty());

        self.signed_out.set(status.user.is_none());
        self.list_hint.set_visible(status.user.is_none());
        self.update_body();
    }

    /// Show the status page while signed out with no rows, the list otherwise.
    fn update_body(&self) {
        let page = if self.signed_out.get() && self.store.model().n_items() == 0 {
            "signed-out"
        } else {
            "list"
        };
        self.body.set_visible_child_name(page);
    }
}

/// The arguments of `App::remove` for `row`: its key and live `on_loan`. `None` when the
/// row is gone or has no folder.
fn remove_args(row: Option<&Row>) -> Option<(String, bool)> {
    let row = row?;
    row.folder.as_ref()?;
    Some((row.key.clone(), row.on_loan))
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A "Sign in" button. Task 4 connects it to the sign-in dialog.
fn sign_in_button() -> gtk::Button {
    gtk::Button::builder()
        .label("Sign in")
        .halign(gtk::Align::Start)
        .css_classes(["suggested-action"])
        .build()
}

fn set_spinning(spinner: &gtk::Spinner, spinning: bool) {
    spinner.set_visible(spinning);
    spinner.set_spinning(spinning);
}

/// Row widgets over the store's `RowObject`s. Bound widgets reload their cover when
/// `covers` reports it ready.
fn row_factory(covers: &Rc<Covers>) -> gtk::SignalListItemFactory {
    let bound: Rc<RefCell<Vec<RowWidget>>> = Rc::default();
    covers.connect_ready(glib::clone!(
        #[strong]
        bound,
        move |path| {
            for widget in bound.borrow().iter() {
                widget.cover_ready(path);
            }
        }
    ));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
        item.set_activatable(false);
        item.set_child(Some(&RowWidget::new()));
    });
    factory.connect_bind(glib::clone!(
        #[strong]
        covers,
        #[strong]
        bound,
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
            let (Some(object), Some(widget)) = (
                item.item().and_downcast::<RowObject>(),
                item.child().and_downcast::<RowWidget>(),
            ) else {
                return;
            };
            widget.bind(&object, &covers);
            let mut bound = bound.borrow_mut();
            if !bound.contains(&widget) {
                bound.push(widget);
            }
        }
    ));
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
        if let Some(widget) = item.child().and_downcast::<RowWidget>() {
            widget.unbind();
            bound.borrow_mut().retain(|other| other != &widget);
        }
    });
    factory
}

/// Let the user pick an ACSM file and import it.
fn choose_acsm(window: &adw::ApplicationWindow, core: Arc<App>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("ACSM"));
    filter.add_pattern("*.acsm");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .filters(&filters)
        .default_filter(&filter)
        .modal(true)
        .build();
    dialog.open(Some(window), gio::Cancellable::NONE, move |result| {
        // A dismissed dialog reports an error; there is nothing to import then.
        if let Some(path) = result.ok().and_then(|file| file.path()) {
            core.import_acsm(path.to_string_lossy().into_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafport_app::{Activity, RowState};

    fn row(folder: Option<&str>, on_loan: bool) -> Row {
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
            on_loan,
            activity: Activity::Idle,
            primary: None,
            menu: Vec::new(),
        }
    }

    #[test]
    fn remove_args_skips_gone_row() {
        assert_eq!(remove_args(None), None);
    }

    #[test]
    fn remove_args_skips_row_without_folder() {
        assert_eq!(remove_args(Some(&row(None, false))), None);
    }

    #[test]
    fn remove_args_uses_live_on_loan() {
        assert_eq!(
            remove_args(Some(&row(Some("/b/t"), false))),
            Some(("k1".to_owned(), false))
        );
        assert_eq!(
            remove_args(Some(&row(Some("/b/t"), true))),
            Some(("k1".to_owned(), true))
        );
    }
}
