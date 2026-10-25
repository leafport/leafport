//! The short messages a UI shows once work finishes, such as a toast.

/// What finished since the sync and import queue was last empty.
#[derive(Default, Debug)]
pub(crate) struct Batch {
    /// Titles of the books synced.
    pub synced: Vec<String>,
    /// Titles of the books imported.
    pub imported: Vec<String>,
    /// How many syncs failed. Cancelled syncs are not counted.
    pub failed: usize,
}

impl Batch {
    /// The message for this batch, or `None` when nothing finished.
    pub fn message(&self) -> Option<String> {
        match (
            self.synced.as_slice(),
            self.imported.as_slice(),
            self.failed,
        ) {
            ([], [], 0) => None,
            ([title], [], 0) => Some(format!("“{title}” synced")),
            ([], [title], 0) => Some(format!("“{title}” imported")),
            ([], [], 1) => Some("1 book failed to sync".into()),
            ([], [], failed) => Some(format!("{failed} books failed to sync")),
            (synced, imported, failed) => {
                let parts: Vec<String> = [
                    (synced.len(), "synced"),
                    (imported.len(), "imported"),
                    (failed, "failed"),
                ]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .enumerate()
                .map(|(i, (count, verb))| match (i, count) {
                    (0, 1) => format!("1 book {verb}"),
                    (0, _) => format!("{count} books {verb}"),
                    _ => format!("{count} {verb}"),
                })
                .collect();
                Some(parts.join(", "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(synced: &[&str], imported: &[&str], failed: usize) -> Option<String> {
        Batch {
            synced: synced.iter().map(|t| t.to_string()).collect(),
            imported: imported.iter().map(|t| t.to_string()).collect(),
            failed,
        }
        .message()
    }

    #[test]
    fn messages() {
        assert_eq!(batch(&[], &[], 0), None);
        assert_eq!(batch(&["A"], &[], 0).unwrap(), "“A” synced");
        assert_eq!(batch(&[], &["A"], 0).unwrap(), "“A” imported");
        assert_eq!(batch(&[], &[], 2).unwrap(), "2 books failed to sync");
        assert_eq!(batch(&["A", "B"], &[], 0).unwrap(), "2 books synced");
        assert_eq!(
            batch(&["A", "B"], &["C"], 1).unwrap(),
            "2 books synced, 1 imported, 1 failed"
        );
        assert_eq!(batch(&["A"], &[], 1).unwrap(), "1 book synced, 1 failed");
        assert_eq!(batch(&[], &["C"], 1).unwrap(), "1 book imported, 1 failed");
    }
}
