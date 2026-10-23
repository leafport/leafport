//! Public types of the app crate. They only use shapes that UniFFI can carry.

use std::fmt;

/// The lifecycle state shown for a row.
#[derive(Clone, Debug, PartialEq)]
pub enum RowState {
    Available,
    Synced,
    OnLoan,
    Returned,
    Local,
    Failed,
}

/// What a row is doing right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Activity {
    Idle,
    Queued,
    Syncing { cancellable: bool },
    Returning,
}

/// The row's main button. `None` in `Row::primary` means no button.
#[derive(Clone, Debug, PartialEq)]
pub enum Primary {
    Open,
    Sync,
    Retry,
}

/// An entry of the row's menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuAction {
    CancelSync { enabled: bool },
    SyncAgain,
    ReturnLoan,
    ShowInFolder,
    RemoveFromDisk,
}

/// The color theme setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// One title in the list, with everything a UI needs to draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub title: String,
    pub author: Option<String>,
    /// `Format::label()`: "EPUB", "PDF" and so on.
    pub format: String,
    pub state: RowState,
    /// Loan expiry as stored, set only while on loan.
    pub expires: Option<String>,
    pub cover_path: Option<String>,
    pub folder: Option<String>,
    pub publication: Option<String>,
    pub error: Option<String>,
    pub on_loan: bool,
    pub activity: Activity,
    pub primary: Option<Primary>,
    pub menu: Vec<MenuAction>,
}

/// App-wide state outside the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub user: Option<String>,
    pub signing_in: bool,
    pub loading_providers: bool,
    pub refreshing: bool,
    pub importing: bool,
    pub job_running: bool,
    pub can_sync_all: bool,
    pub can_cancel_all: bool,
    pub sign_in_error: Option<String>,
    pub banner: Vec<String>,
    /// The effective books folder.
    pub books_dir: Option<String>,
}

/// A sign-in provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub name: String,
    pub method: String,
    pub node_id: Option<String>,
    pub sign_in_type: Option<String>,
}

/// User settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub books_dir: Option<String>,
    pub theme: Theme,
}

/// The whole UI state at one point in time.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub status: Status,
    pub settings: Settings,
}

/// A change pushed to the `Listener`.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Rows {
        rows: Vec<Row>,
    },
    Row {
        row: Row,
    },
    RowRemoved {
        key: String,
    },
    Progress {
        key: String,
        fraction: f32,
    },
    Status {
        status: Status,
    },
    Providers {
        result: Result<Vec<Provider>, String>,
    },
}

/// Why a call was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum AppError {
    /// Another operation is already running.
    Busy,
    /// Saving failed.
    Save { message: String },
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("another operation is running"),
            Self::Save { message } => write!(f, "could not save: {message}"),
        }
    }
}

impl std::error::Error for AppError {}

/// Receives events from the reactor thread.
pub trait Listener: Send + Sync {
    fn on_event(&self, event: Event);
}
