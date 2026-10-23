//! UI-free application logic shared by the native Leafport frontends.

// Consumed by the reactor added in a later task.
#[allow(dead_code)]
mod rows;
mod types;

pub use types::{
    Activity, AppError, Event, Listener, MenuAction, Primary, Provider, Row, RowState, Settings,
    Snapshot, Status, Theme,
};
