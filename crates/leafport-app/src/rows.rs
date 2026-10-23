//! Row rules: which state, button and menu entries each item gets.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use leafport_core::library::{Item, ItemState};

use crate::{Activity, MenuAction, Primary, Row, RowState, Status};

/// A unit of queued work. One runs at a time, in request order.
// The sync queue constructs these; until it lands only the row rules read them.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Job {
    /// Sync the row with this key.
    Sync(String),
    /// Import the ACSM file at this path.
    Import(PathBuf),
}

/// Which rows are queued, syncing or returning, and the last return error per row.
pub(crate) struct Busy<'a> {
    pub running: Option<&'a Job>,
    /// Whether the running sync can still be cancelled, i.e. its Fulfill was not sent.
    /// It describes the running sync only.
    pub cancellable: bool,
    pub queue: &'a VecDeque<Job>,
    pub returning: &'a HashSet<String>,
    pub row_errors: &'a HashMap<String, String>,
}

impl Busy<'_> {
    fn syncing(&self, key: &str) -> bool {
        matches!(self.running, Some(Job::Sync(k)) if k == key)
    }

    fn queued(&self, key: &str) -> bool {
        self.queue
            .iter()
            .any(|job| matches!(job, Job::Sync(k) if k == key))
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build the row a UI draws for `item`.
pub(crate) fn row(item: &Item, busy: &Busy) -> Row {
    let key = item.key.as_str();
    // A loan can be returned without a cloud record, so local loan folders offer it too.
    let on_loan = matches!(item.state, ItemState::OnLoan { .. })
        || item.metadata.as_ref().is_some_and(|m| m.loan.is_some());
    let resync = item.publication.is_some()
        && item.acsm.is_some()
        && !matches!(item.state, ItemState::Returned);

    let syncing = busy.syncing(key);
    let queued = busy.queued(key);
    let returning = busy.returning.contains(key);

    let activity = if syncing {
        Activity::Syncing {
            cancellable: busy.cancellable,
        }
    } else if queued {
        Activity::Queued
    } else if returning {
        Activity::Returning
    } else {
        Activity::Idle
    };

    // A queued or running sync offers only Cancel; a loan return cannot be cancelled.
    let menu = if syncing || queued {
        vec![MenuAction::CancelSync {
            enabled: queued || busy.cancellable,
        }]
    } else if returning {
        Vec::new()
    } else {
        let mut menu = Vec::new();
        if resync {
            menu.push(MenuAction::SyncAgain);
        }
        if on_loan {
            menu.push(MenuAction::ReturnLoan);
        }
        if item.folder.is_some() {
            menu.push(MenuAction::ShowInFolder);
            menu.push(MenuAction::RemoveFromDisk);
        }
        menu
    };

    let primary = match (&item.publication, &item.state) {
        (Some(_), _) => Some(Primary::Open),
        (None, _) if item.acsm.is_none() => None,
        (None, ItemState::Failed(_)) => Some(Primary::Retry),
        (None, _) => Some(Primary::Sync),
    };

    let (state, expires, failure) = match &item.state {
        ItemState::Available => (RowState::Available, None, None),
        ItemState::Synced => (RowState::Synced, None, None),
        ItemState::OnLoan { expires } => (RowState::OnLoan, expires.clone(), None),
        ItemState::Returned => (RowState::Returned, None, None),
        ItemState::Local => (RowState::Local, None, None),
        ItemState::Failed(message) => (RowState::Failed, None, Some(message.clone())),
    };

    Row {
        key: item.key.clone(),
        title: item.title.clone(),
        author: item.author.clone(),
        format: item.format.label().to_owned(),
        state,
        expires,
        cover_path: item.cover.as_deref().map(path_string),
        folder: item.folder.as_deref().map(path_string),
        publication: item.publication.as_deref().map(path_string),
        error: failure.or_else(|| busy.row_errors.get(key).cloned()),
        on_loan,
        activity,
        primary,
        menu,
    }
}

/// The app-wide facts the status line is built from.
pub(crate) struct StatusInputs<'a> {
    /// The signed-in account label.
    pub user: Option<&'a str>,
    pub refreshing: bool,
    pub banner: &'a [String],
    /// The effective books folder; `None` when none could be determined.
    pub books_dir: Option<&'a Path>,
}

/// Build the status a UI draws outside the list.
pub(crate) fn status(inputs: &StatusInputs) -> Status {
    Status {
        user: inputs.user.map(str::to_owned),
        signing_in: false,
        loading_providers: false,
        refreshing: inputs.refreshing,
        importing: false,
        job_running: false,
        can_sync_all: false,
        can_cancel_all: false,
        sign_in_error: None,
        banner: inputs.banner.to_vec(),
        books_dir: inputs.books_dir.map(path_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Activity, MenuAction, Primary, RowState};
    use leafport_core::library::{Format, Item, ItemState, Loan, Metadata, Source};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::path::PathBuf;

    fn item(state: ItemState) -> Item {
        Item {
            key: "k1".into(),
            title: "Title".into(),
            author: None,
            format: Format::Epub,
            state,
            acsm: Some(b"acsm".to_vec()),
            folder: None,
            publication: None,
            cover: None,
            metadata: None,
            warning: None,
        }
    }

    fn synced() -> Item {
        let mut i = item(ItemState::Synced);
        i.publication = Some(PathBuf::from("/b/t/book.epub"));
        i.folder = Some(PathBuf::from("/b/t"));
        i
    }

    struct Env {
        queue: VecDeque<Job>,
        returning: HashSet<String>,
        errors: HashMap<String, String>,
    }

    impl Env {
        fn new() -> Env {
            Env {
                queue: VecDeque::new(),
                returning: HashSet::new(),
                errors: HashMap::new(),
            }
        }

        fn row(&self, item: &Item, running: Option<&Job>, cancellable: bool) -> crate::Row {
            row(
                item,
                &Busy {
                    running,
                    cancellable,
                    queue: &self.queue,
                    returning: &self.returning,
                    row_errors: &self.errors,
                },
            )
        }
    }

    #[test]
    fn status_copies_inputs() {
        let banner = vec!["m".to_string()];
        let s = status(&StatusInputs {
            user: Some("u"),
            refreshing: true,
            banner: &banner,
            books_dir: Some(Path::new("/b")),
        });
        assert_eq!(s.user.as_deref(), Some("u"));
        assert!(s.refreshing);
        assert_eq!(s.banner, banner);
        assert_eq!(s.books_dir.as_deref(), Some("/b"));
        assert!(!s.job_running && !s.signing_in && s.sign_in_error.is_none());
    }

    #[test]
    fn available_with_acsm_offers_sync() {
        let r = Env::new().row(&item(ItemState::Available), None, false);
        assert_eq!(r.primary, Some(Primary::Sync));
        assert!(r.menu.is_empty());
        assert_eq!(r.activity, Activity::Idle);
        assert_eq!(r.format, "EPUB");
    }

    #[test]
    fn available_without_acsm_has_no_primary() {
        let mut i = item(ItemState::Available);
        i.acsm = None;
        assert_eq!(Env::new().row(&i, None, false).primary, None);
    }

    #[test]
    fn failed_offers_retry_and_error() {
        let r = Env::new().row(&item(ItemState::Failed("boom".into())), None, false);
        assert_eq!(r.primary, Some(Primary::Retry));
        assert_eq!(r.state, RowState::Failed);
        assert_eq!(r.error.as_deref(), Some("boom"));
    }

    #[test]
    fn synced_offers_open_and_menu() {
        let r = Env::new().row(&synced(), None, false);
        assert_eq!(r.primary, Some(Primary::Open));
        assert_eq!(
            r.menu,
            [
                MenuAction::SyncAgain,
                MenuAction::ShowInFolder,
                MenuAction::RemoveFromDisk
            ]
        );
        assert_eq!(r.publication.as_deref(), Some("/b/t/book.epub"));
        assert_eq!(r.folder.as_deref(), Some("/b/t"));
    }

    #[test]
    fn returned_has_no_sync_again() {
        let mut i = synced();
        i.state = ItemState::Returned;
        let r = Env::new().row(&i, None, false);
        assert!(!r.menu.contains(&MenuAction::SyncAgain));
        assert_eq!(r.state, RowState::Returned);
    }

    #[test]
    fn local_loan_metadata_offers_return() {
        let mut i = item(ItemState::Local);
        i.metadata = Some(Metadata {
            resource: "k1".into(),
            title: "Title".into(),
            author: None,
            format: "epub".into(),
            fulfillment: None,
            source: Source::Import,
            synced_at: "2026-10-01T00:00:00Z".into(),
            returned: false,
            loan: Some(Loan {
                operator_url: "https://op".into(),
                expires: None,
                token: "t".into(),
            }),
        });
        let r = Env::new().row(&i, None, false);
        assert!(r.on_loan);
        assert!(r.menu.contains(&MenuAction::ReturnLoan));
        assert_eq!(r.state, RowState::Local);
    }

    #[test]
    fn on_loan_sets_expires() {
        let i = item(ItemState::OnLoan {
            expires: Some("2026-11-01".into()),
        });
        let r = Env::new().row(&i, None, false);
        assert_eq!(r.expires.as_deref(), Some("2026-11-01"));
        assert_eq!(r.state, RowState::OnLoan);
        assert!(r.on_loan);
    }

    #[test]
    fn queued_menu_is_cancel_only() {
        let mut env = Env::new();
        env.queue.push_back(Job::Sync("k1".into()));
        let r = env.row(&synced(), None, false);
        assert_eq!(r.activity, Activity::Queued);
        assert_eq!(r.menu, [MenuAction::CancelSync { enabled: true }]);
    }

    #[test]
    fn running_committed_cannot_cancel() {
        let job = Job::Sync("k1".into());
        let r = Env::new().row(&synced(), Some(&job), false);
        assert_eq!(r.activity, Activity::Syncing { cancellable: false });
        assert_eq!(r.menu, [MenuAction::CancelSync { enabled: false }]);
        let r = Env::new().row(&synced(), Some(&job), true);
        assert_eq!(r.menu, [MenuAction::CancelSync { enabled: true }]);
    }

    #[test]
    fn returning_row() {
        let mut env = Env::new();
        env.returning.insert("k1".into());
        let r = env.row(&synced(), None, false);
        assert_eq!(r.activity, Activity::Returning);
        assert!(r.menu.is_empty());
    }

    #[test]
    fn return_error_shown_without_failed_state() {
        let mut env = Env::new();
        env.errors.insert("k1".into(), "nope".into());
        let i = item(ItemState::OnLoan { expires: None });
        let r = env.row(&i, None, false);
        assert_eq!(r.state, RowState::OnLoan);
        assert_eq!(r.error.as_deref(), Some("nope"));
    }
}
