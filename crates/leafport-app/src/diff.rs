//! The events that turn one row list into the next.

use crate::{Event, Row};

/// The events that turn `old` into `new`.
///
/// When `new` keeps the order of `old` and only drops rows, the result is one
/// `RowRemoved` per dropped row, then one `Row` per changed row. Any insert or
/// reorder sends the whole list as `Rows`.
pub(crate) fn diff(old: &[Row], new: &[Row]) -> Vec<Event> {
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut next = new.iter().peekable();
    for row in old {
        match next.peek() {
            Some(kept) if kept.key == row.key => {
                if *kept != row {
                    changed.push(Event::Row {
                        row: (*kept).clone(),
                    });
                }
                next.next();
            }
            _ => removed.push(Event::RowRemoved {
                key: row.key.clone(),
            }),
        }
    }
    if next.next().is_some() {
        return vec![Event::Rows { rows: new.to_vec() }];
    }
    removed.extend(changed);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Activity, RowState};

    fn row(key: &str, title: &str) -> Row {
        Row {
            key: key.into(),
            title: title.into(),
            author: None,
            format: "EPUB".into(),
            state: RowState::Available,
            expires: None,
            cover_path: None,
            folder: None,
            publication: None,
            error: None,
            warning: None,
            on_loan: false,
            activity: Activity::Idle,
            primary: None,
            menu: Vec::new(),
        }
    }

    #[test]
    fn same_rows_no_events() {
        let (a, b) = (row("a", "A"), row("b", "B"));
        assert!(diff(&[a.clone(), b.clone()], &[a, b]).is_empty());
    }

    #[test]
    fn changed_row_emits_row() {
        let (a, b) = (row("a", "A"), row("b", "B"));
        let b2 = row("b", "B2");
        assert_eq!(
            diff(&[a.clone(), b], &[a, b2.clone()]),
            [Event::Row { row: b2 }]
        );
    }

    #[test]
    fn one_removed_emits_row_removed() {
        let (a, b, c) = (row("a", "A"), row("b", "B"), row("c", "C"));
        assert_eq!(
            diff(&[a.clone(), b, c.clone()], &[a, c]),
            [Event::RowRemoved { key: "b".into() }]
        );
    }

    #[test]
    fn removed_and_changed() {
        let (a, b, c) = (row("a", "A"), row("b", "B"), row("c", "C"));
        let a2 = row("a", "A2");
        assert_eq!(
            diff(&[a, b, c.clone()], &[a2.clone(), c]),
            [
                Event::RowRemoved { key: "b".into() },
                Event::Row { row: a2 }
            ]
        );
    }

    #[test]
    fn insert_or_reorder_emits_rows() {
        let (a, b) = (row("a", "A"), row("b", "B"));
        assert_eq!(
            diff(std::slice::from_ref(&a), &[a.clone(), b.clone()]),
            [Event::Rows {
                rows: vec![a.clone(), b.clone()]
            }]
        );
        assert_eq!(
            diff(&[a.clone(), b.clone()], &[b.clone(), a.clone()]),
            [Event::Rows { rows: vec![b, a] }]
        );
    }

    #[test]
    fn all_removed_emits_row_removed() {
        let (a, b) = (row("a", "A"), row("b", "B"));
        assert_eq!(
            diff(&[a, b], &[]),
            [
                Event::RowRemoved { key: "a".into() },
                Event::RowRemoved { key: "b".into() }
            ]
        );
    }
}
