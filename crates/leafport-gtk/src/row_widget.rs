//! One list row: cover, text, primary button and menu button.

use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use leafport_app::{Activity, MenuAction, Primary, Row, RowState};

use crate::covers::Covers;
use crate::row_menu::menu_model;
use crate::store::RowObject;

/// Row height, as egui's `ROW_HEIGHT`.
const ROW_HEIGHT: i32 = 72;
/// Cover size, as egui's `COVER`.
const COVER_WIDTH: i32 = 48;
const COVER_HEIGHT: i32 = 64;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct RowWidget {
        pub cover: gtk::Stack,
        pub picture: gtk::Picture,
        pub placeholder: gtk::Label,
        pub title: gtk::Label,
        pub info: gtk::Label,
        pub note: gtk::Label,
        pub primary: gtk::Stack,
        pub button: gtk::Button,
        pub spinner: gtk::Spinner,
        pub bar: gtk::ProgressBar,
        pub menu: gtk::MenuButton,
        pub object: RefCell<Option<RowObject>>,
        pub covers: RefCell<Option<Rc<Covers>>>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub pulse: RefCell<Option<glib::SourceId>>,
        /// What the menu model was built from, so an update that keeps it does not
        /// replace the model and close an open menu.
        pub menu_shown: RefCell<Option<MenuInputs>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowWidget {
        const NAME: &'static str = "LeafportRowWidget";
        type Type = super::RowWidget;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RowWidget {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }

        fn dispose(&self) {
            self.obj().stop_pulse();
        }
    }

    impl WidgetImpl for RowWidget {}
    impl BoxImpl for RowWidget {}
}

glib::wrapper! {
    /// The widget of one `RowObject`.
    pub struct RowWidget(ObjectSubclass<imp::RowWidget>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for RowWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl RowWidget {
    /// An unbound row.
    pub fn new() -> Self {
        glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 12)
            .build()
    }

    fn build(&self) {
        let imp = self.imp();
        self.set_height_request(ROW_HEIGHT);
        self.set_margin_start(12);
        self.set_margin_end(12);

        imp.picture.set_content_fit(gtk::ContentFit::Contain);
        imp.picture.set_can_shrink(true);
        imp.placeholder.add_css_class("caption");
        imp.placeholder.add_css_class("dim-label");
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&imp.placeholder));
        imp.cover.add_named(&imp.picture, Some("picture"));
        imp.cover.add_named(&frame, Some("placeholder"));
        let cover = CoverBox::new(&imp.cover);
        cover.set_valign(gtk::Align::Center);
        self.append(&cover);

        imp.title.add_css_class("heading");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        for label in [&imp.title, &imp.info, &imp.note] {
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_selectable(true);
            // A selectable label takes focus; keep Tab moving between the row buttons.
            label.set_focusable(false);
            text.append(label);
        }
        self.append(&text);

        imp.spinner.set_halign(gtk::Align::Center);
        imp.bar.set_valign(gtk::Align::Center);
        imp.bar.set_pulse_step(0.1);
        imp.bar.add_css_class(PROGRESS_CLASS);
        imp.primary.add_named(&imp.button, Some("button"));
        imp.primary.add_named(&imp.spinner, Some("spinner"));
        imp.primary.add_named(&imp.bar, Some("bar"));
        // The stack is as wide as its widest page: the button at its natural width,
        // since the bar's trough has no minimum width (see `load_css`).
        imp.primary.set_valign(gtk::Align::Center);
        self.append(&imp.primary);

        imp.menu.set_icon_name("view-more-symbolic");
        imp.menu.set_has_frame(false);
        imp.menu.set_valign(gtk::Align::Center);
        imp.menu.set_tooltip_text(Some("More"));
        self.append(&imp.menu);
    }

    /// Show `object` and follow its changes until `unbind`.
    pub fn bind(&self, object: &RowObject, covers: &Rc<Covers>) {
        self.unbind();
        let imp = self.imp();
        *imp.object.borrow_mut() = Some(object.clone());
        *imp.covers.borrow_mut() = Some(covers.clone());
        let version = object.connect_notify_local(
            Some("version"),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.update()
            ),
        );
        let progress = object.connect_notify_local(
            Some("progress"),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.update_progress()
            ),
        );
        imp.handlers.replace(vec![version, progress]);
        self.update();
    }

    /// Stop following the bound object.
    pub fn unbind(&self) {
        let imp = self.imp();
        self.stop_pulse();
        if let Some(object) = imp.object.take() {
            for handler in imp.handlers.take() {
                object.disconnect(handler);
            }
        }
        imp.covers.take();
        imp.menu_shown.take();
    }

    /// Reload the picture when it shows `path`.
    pub fn cover_ready(&self, path: &str) {
        if self.row().and_then(|row| row.cover_path).as_deref() == Some(path) {
            self.update_cover();
        }
    }

    fn row(&self) -> Option<Row> {
        self.imp().object.borrow().as_ref().map(RowObject::row)
    }

    /// Rebuild text, buttons, menu and cover from the bound row.
    fn update(&self) {
        let Some(row) = self.row() else {
            return;
        };
        let imp = self.imp();

        imp.title.set_label(&row.title);
        imp.info.set_label(&info_line(&row));
        let (note, error) = match (&row.error, &row.warning) {
            (Some(error), _) => (Some(error), true),
            (None, Some(warning)) => (Some(warning), false),
            (None, None) => (None, false),
        };
        imp.note.set_label(note.map_or("", String::as_str));
        // The note is ellipsized to one line; the tooltip holds all of it.
        imp.note.set_tooltip_text(note.map(String::as_str));
        imp.note.set_visible(note.is_some());
        imp.note
            .set_css_classes(if error { &["error"] } else { &["dim-label"] });

        let (label, action) = primary(&row);
        imp.button.set_label(label);
        match action {
            Some(action) => {
                imp.button.set_sensitive(true);
                imp.button
                    .set_action_target_value(Some(&row.key.to_variant()));
                imp.button.set_action_name(Some(action));
            }
            None => {
                imp.button.set_action_name(None);
                imp.button.set_sensitive(false);
            }
        }
        let page = match row.activity {
            Activity::Idle => "button",
            Activity::Queued | Activity::Returning => "spinner",
            Activity::Syncing { .. } => "bar",
        };
        imp.primary.set_visible_child_name(page);
        // An excluded title has nothing to sync; its menu lifts the exclusion.
        imp.primary
            .set_visible(!(row.state == RowState::Excluded && row.primary.is_none()));
        imp.spinner.set_spinning(page == "spinner");

        let inputs = MenuInputs::of(&row);
        if imp.menu_shown.borrow().as_ref() != Some(&inputs) {
            imp.menu.set_menu_model(Some(&menu_model(&row)));
            imp.menu_shown.replace(Some(inputs));
        }
        imp.menu.set_sensitive(!row.menu.is_empty());

        self.update_progress();
        self.update_cover();
    }

    /// Show the bound row's fraction, or pulse while a sync has none.
    fn update_progress(&self) {
        let imp = self.imp();
        let Some(object) = imp.object.borrow().clone() else {
            return;
        };
        let syncing = matches!(object.row().activity, Activity::Syncing { .. });
        match object.progress() {
            Some(fraction) if syncing => {
                self.stop_pulse();
                imp.bar.set_fraction(f64::from(fraction.clamp(0.0, 1.0)));
            }
            None if syncing => {
                if imp.pulse.borrow().is_none() {
                    let bar = imp.bar.downgrade();
                    let source = glib::timeout_add_local(Duration::from_millis(100), move || {
                        match bar.upgrade() {
                            Some(bar) => {
                                bar.pulse();
                                glib::ControlFlow::Continue
                            }
                            None => glib::ControlFlow::Break,
                        }
                    });
                    imp.pulse.replace(Some(source));
                }
            }
            _ => self.stop_pulse(),
        }
    }

    fn stop_pulse(&self) {
        if let Some(source) = self.imp().pulse.take() {
            source.remove();
        }
    }

    fn update_cover(&self) {
        let imp = self.imp();
        let Some(row) = self.row() else {
            return;
        };
        let texture = row.cover_path.as_deref().and_then(|path| {
            let covers = imp.covers.borrow().clone()?;
            covers.get(path)
        });
        match texture {
            Some(texture) => {
                imp.picture.set_paintable(Some(&texture));
                imp.cover.set_visible_child_name("picture");
            }
            None => {
                imp.picture.set_paintable(gtk::gdk::Paintable::NONE);
                imp.placeholder.set_label(&row.format);
                imp.cover.set_visible_child_name("placeholder");
            }
        }
    }
}

/// The CSS class of the row's progress bar.
const PROGRESS_CLASS: &str = "leafport-row-progress";

/// Let the row's progress bar take the button's width. Adwaita gives a horizontal
/// trough a 150 px minimum, which would widen every row's button to 150 px.
pub fn load_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    // `load_from_string` needs GTK 4.12; the minimum is 4.10.
    provider.load_from_data(&format!(
        "progressbar.{PROGRESS_CLASS} > trough {{ min-width: 0; }}"
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

mod cover_imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct CoverBox {
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CoverBox {
        const NAME: &'static str = "LeafportCoverBox";
        type Type = super::CoverBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CoverBox {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CoverBox {
        /// The cover size, or the child's minimum when that is larger. A picture's
        /// natural size (the 96×128 thumbnail) is ignored, so it does not grow the row.
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = match orientation {
                gtk::Orientation::Horizontal => COVER_WIDTH,
                _ => COVER_HEIGHT,
            };
            let minimum = self
                .child
                .borrow()
                .as_ref()
                .filter(|child| child.should_layout())
                .map_or(0, |child| child.measure(orientation, -1).0);
            let size = size.max(minimum);
            (size, size, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref()
                && child.should_layout()
            {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}

glib::wrapper! {
    /// Holds the cover at exactly 48×64, unless its child needs more.
    pub struct CoverBox(ObjectSubclass<cover_imp::CoverBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CoverBox {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let this: Self = glib::Object::new();
        child.set_parent(&this);
        *this.imp().child.borrow_mut() = Some(child.clone().upcast());
        this
    }
}

/// The parts of a row that `menu_model` reads.
#[derive(Debug, PartialEq)]
pub struct MenuInputs {
    key: String,
    menu: Vec<MenuAction>,
    has_folder: bool,
}

impl MenuInputs {
    fn of(row: &Row) -> Self {
        Self {
            key: row.key.clone(),
            menu: row.menu.clone(),
            has_folder: row.folder.is_some(),
        }
    }
}

/// The state label, as in egui's `list.rs::state_label`.
fn state_label(state: &RowState) -> &'static str {
    match state {
        RowState::Available => "Available",
        RowState::Synced => "Synced",
        RowState::OnLoan => "On loan",
        RowState::Returned => "Returned",
        RowState::Local => "Local",
        RowState::Excluded => "Excluded",
        RowState::Failed => "Failed",
    }
}

/// `"{format} · {state}"`, plus `" · until {date}"` while on loan.
fn info_line(row: &Row) -> String {
    let mut info = format!("{} · {}", row.format, state_label(&row.state));
    if row.state == RowState::OnLoan
        && let Some(expires) = &row.expires
    {
        info.push_str(&format!(
            " · until {}",
            expires.get(..10).unwrap_or(expires)
        ));
    }
    info
}

/// The primary button's label and action. `None` keeps the button disabled.
fn primary(row: &Row) -> (&'static str, Option<&'static str>) {
    match &row.primary {
        Some(Primary::Open) => ("Open", row.publication.as_ref().map(|_| "row.open")),
        Some(Primary::Retry) => ("Retry", Some("row.sync")),
        Some(Primary::Sync) => ("Sync", Some("row.sync")),
        // A row without an ACSM keeps a disabled button, labelled as it would be.
        None if row.state == RowState::Failed => ("Retry", None),
        None => ("Sync", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            key: "k".into(),
            title: "t".into(),
            author: None,
            format: "EPUB".into(),
            state: RowState::Available,
            expires: None,
            cover_path: None,
            folder: Some("/b/t".into()),
            publication: None,
            error: None,
            warning: None,
            on_loan: false,
            activity: Activity::Idle,
            primary: Some(Primary::Sync),
            menu: vec![MenuAction::ShowInFolder],
        }
    }

    #[test]
    fn menu_inputs_ignore_fields_the_menu_does_not_show() {
        let mut changed = row();
        changed.title = "other".into();
        changed.activity = Activity::Queued;
        assert_eq!(MenuInputs::of(&row()), MenuInputs::of(&changed));
    }

    #[test]
    fn menu_inputs_follow_entries_key_and_folder() {
        let base = MenuInputs::of(&row());
        let mut entries = row();
        entries.menu = vec![MenuAction::CancelSync { enabled: true }];
        let mut key = row();
        key.key = "other".into();
        let mut folder = row();
        folder.folder = None;
        for changed in [entries, key, folder] {
            assert_ne!(MenuInputs::of(&changed), base);
        }
    }

    #[test]
    fn info_line_shows_expiry_only_on_loan() {
        let mut loaned = row();
        loaned.state = RowState::OnLoan;
        loaned.expires = Some("2026-11-02T10:00:00Z".into());
        assert_eq!(info_line(&loaned), "EPUB · On loan · until 2026-11-02");
        let mut returned = loaned.clone();
        returned.state = RowState::Returned;
        assert_eq!(info_line(&returned), "EPUB · Returned");
    }

    #[test]
    fn primary_keeps_a_disabled_button_without_an_action() {
        let mut failed = row();
        failed.primary = None;
        failed.state = RowState::Failed;
        assert_eq!(primary(&failed), ("Retry", None));
        let mut open = row();
        open.primary = Some(Primary::Open);
        assert_eq!(primary(&open), ("Open", None));
        open.publication = Some("/b/t/book.epub".into());
        assert_eq!(primary(&open), ("Open", Some("row.open")));
    }
}
