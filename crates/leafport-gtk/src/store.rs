//! The row list as a `gio::ListStore` of `RowObject`s, without any widgets.

use std::collections::HashMap;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use leafport_app::{Activity, Row};

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::RowObject, ext_trait)]
    pub struct RowObject {
        pub row: RefCell<Option<Row>>,
        /// Counts `set_row` calls so widgets can rebind on `notify::version`.
        #[property(get)]
        pub version: Cell<u32>,
        /// Fraction in `0.0..=1.0` while a row syncs; negative means none.
        #[property(get, set)]
        pub progress: Cell<f64>,
    }

    impl Default for RowObject {
        fn default() -> Self {
            Self {
                row: RefCell::new(None),
                version: Cell::new(0),
                progress: Cell::new(-1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowObject {
        const NAME: &'static str = "LeafportRowObject";
        type Type = super::RowObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for RowObject {}
}

glib::wrapper! {
    /// One list entry: the latest `Row` plus its sync progress.
    pub struct RowObject(ObjectSubclass<imp::RowObject>);
}

impl RowObject {
    /// Wrap `row`.
    pub fn new(row: Row) -> Self {
        let object: Self = glib::Object::new();
        *object.imp().row.borrow_mut() = Some(row);
        object
    }

    /// A clone of the current row.
    pub fn row(&self) -> Row {
        self.imp()
            .row
            .borrow()
            .clone()
            .expect("RowObject is built with a row")
    }

    /// The row's key.
    pub fn key(&self) -> String {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .map(|row| row.key.clone())
            .expect("RowObject is built with a row")
    }

    /// Replace the row and notify `version`. Progress is cleared when the row is no
    /// longer syncing.
    pub fn set_row(&self, row: Row) {
        let syncing = matches!(row.activity, Activity::Syncing { .. });
        *self.imp().row.borrow_mut() = Some(row);
        if !syncing {
            self.set_progress(None);
        }
        let imp = self.imp();
        imp.version.set(imp.version.get().wrapping_add(1));
        self.notify("version");
    }

    /// The sync progress, if known.
    pub fn progress(&self) -> Option<f32> {
        let value = self.imp().progress.get();
        (value >= 0.0).then_some(value as f32)
    }

    /// Set or clear the sync progress.
    pub fn set_progress(&self, progress: Option<f32>) {
        self.set_property("progress", progress.map_or(-1.0, f64::from));
    }
}

/// The rows as a list model, keeping one `RowObject` per key.
pub struct RowStore {
    model: gio::ListStore,
}

impl Default for RowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RowStore {
    /// An empty store.
    pub fn new() -> Self {
        Self {
            model: gio::ListStore::new::<RowObject>(),
        }
    }

    /// The model to hand to a list view.
    pub fn model(&self) -> gio::ListStore {
        self.model.clone()
    }

    /// Replace the list. Keeps the `RowObject` of every key that stays and calls
    /// `set_row` on it when its row changed; returns the cover paths to forget (see
    /// `stale_cover_paths`).
    pub fn set_rows(&self, rows: Vec<Row>) -> Vec<String> {
        let mut old: HashMap<String, RowObject> = self
            .objects()
            .into_iter()
            .map(|object| (object.key(), object))
            .collect();
        let mut stale = Vec::new();
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            match old.remove(&row.key) {
                Some(object) => {
                    let before = object.row();
                    if before != row {
                        stale.extend(stale_cover_paths(&before, Some(&row)));
                        object.set_row(row);
                    }
                    objects.push(object);
                }
                None => objects.push(RowObject::new(row)),
            }
        }
        for object in old.values() {
            stale.extend(stale_cover_paths(&object.row(), None));
        }
        self.model.splice(0, self.model.n_items(), &objects);
        stale
    }

    /// Replace one row; unknown keys are ignored. Returns cover paths to forget.
    pub fn set_row(&self, row: Row) -> Vec<String> {
        let Some(object) = self.find(&row.key) else {
            return Vec::new();
        };
        let before = object.row();
        if before == row {
            return Vec::new();
        }
        let stale = stale_cover_paths(&before, Some(&row));
        object.set_row(row);
        stale
    }

    /// Remove one row. Returns cover paths to forget.
    pub fn remove(&self, key: &str) -> Vec<String> {
        let Some(position) = (0..self.model.n_items()).find(|&i| {
            self.model
                .item(i)
                .and_downcast::<RowObject>()
                .is_some_and(|object| object.key() == key)
        }) else {
            return Vec::new();
        };
        let stale = self
            .model
            .item(position)
            .and_downcast::<RowObject>()
            .map(|object| stale_cover_paths(&object.row(), None))
            .unwrap_or_default();
        self.model.remove(position);
        stale
    }

    /// Record progress of a syncing row; ignored otherwise.
    pub fn progress(&self, key: &str, fraction: f32) {
        if let Some(object) = self.find(key)
            && matches!(object.row().activity, Activity::Syncing { .. })
        {
            object.set_progress(Some(fraction));
        }
    }

    /// The object of `key`, if listed.
    pub fn find(&self, key: &str) -> Option<RowObject> {
        self.objects()
            .into_iter()
            .find(|object| object.key() == key)
    }

    fn objects(&self) -> Vec<RowObject> {
        (0..self.model.n_items())
            .filter_map(|i| self.model.item(i).and_downcast::<RowObject>())
            .collect()
    }
}

/// `old.folder` and `old.cover_path` (and the same of `new`) when `new` is `None` or its
/// `cover_path` or `folder` differs from `old`; empty otherwise.
pub fn stale_cover_paths(old: &Row, new: Option<&Row>) -> Vec<String> {
    let changed =
        new.is_none_or(|new| new.cover_path != old.cover_path || new.folder != old.folder);
    if !changed {
        return Vec::new();
    }
    [Some(old), new]
        .into_iter()
        .flatten()
        .flat_map(|row| [row.folder.clone(), row.cover_path.clone()])
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafport_app::RowState;

    fn row(key: &str, activity: Activity) -> Row {
        Row {
            key: key.into(),
            title: key.into(),
            author: None,
            format: String::new(),
            state: RowState::Available,
            expires: None,
            cover_path: None,
            folder: None,
            publication: None,
            error: None,
            warning: None,
            on_loan: false,
            activity,
            primary: None,
            menu: Vec::new(),
        }
    }

    fn keys(store: &RowStore) -> Vec<String> {
        let model = store.model();
        (0..model.n_items())
            .map(|i| model.item(i).and_downcast::<RowObject>().unwrap().key())
            .collect()
    }

    #[test]
    fn set_rows_keeps_objects_of_kept_keys() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle), row("b", Activity::Idle)]);
        let obj = store.find("a").unwrap();
        store.set_rows(vec![row("b", Activity::Idle), row("a", Activity::Idle)]);
        assert_eq!(store.find("a").unwrap(), obj);
        assert_eq!(keys(&store), ["b", "a"]);
    }

    #[test]
    fn set_rows_drops_left_rows() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle), row("b", Activity::Idle)]);
        store.set_rows(vec![row("b", Activity::Idle)]);
        assert!(store.find("a").is_none());
        assert_eq!(store.model().n_items(), 1);
    }

    #[test]
    fn set_rows_clears_progress_of_rows_that_stop_syncing() {
        let store = RowStore::new();
        let syncing = Activity::Syncing { cancellable: true };
        store.set_rows(vec![row("a", syncing)]);
        store.progress("a", 0.5);
        assert_eq!(store.find("a").unwrap().progress(), Some(0.5));
        store.set_rows(vec![row("a", Activity::Idle)]);
        assert_eq!(store.find("a").unwrap().progress(), None);
    }

    #[test]
    fn set_rows_keeps_progress_of_rows_still_syncing() {
        let store = RowStore::new();
        let syncing = Activity::Syncing { cancellable: true };
        store.set_rows(vec![row("a", syncing.clone())]);
        store.progress("a", 0.5);
        store.set_rows(vec![row("a", syncing)]);
        assert_eq!(store.find("a").unwrap().progress(), Some(0.5));
    }

    #[test]
    fn progress_ignores_unknown_and_idle_rows() {
        let store = RowStore::new();
        store.progress("x", 0.3);
        store.set_rows(vec![row("a", Activity::Idle)]);
        store.progress("a", 0.3);
        assert_eq!(store.find("a").unwrap().progress(), None);
    }

    #[test]
    fn set_row_ignores_unknown_key() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle)]);
        store.set_row(row("b", Activity::Idle));
        assert_eq!(store.model().n_items(), 1);
        assert!(store.find("b").is_none());
    }

    #[test]
    fn set_row_bumps_version() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle)]);
        let obj = store.find("a").unwrap();
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = count.clone();
        obj.connect_notify_local(Some("version"), move |_, _| seen.set(seen.get() + 1));
        let mut changed = row("a", Activity::Idle);
        changed.title = "changed".into();
        store.set_row(changed);
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn stale_cover_paths_on_change_and_removal() {
        let mut old = row("a", Activity::Idle);
        old.folder = Some("/f".into());
        old.cover_path = Some("/f/c.jpg".into());
        assert!(stale_cover_paths(&old, Some(&old.clone())).is_empty());

        let mut new = old.clone();
        new.folder = Some("/g".into());
        new.cover_path = Some("/g/c.jpg".into());
        assert_eq!(
            stale_cover_paths(&old, Some(&new)),
            ["/f", "/f/c.jpg", "/g", "/g/c.jpg"]
        );
        assert_eq!(stale_cover_paths(&old, None), ["/f", "/f/c.jpg"]);
    }
}
