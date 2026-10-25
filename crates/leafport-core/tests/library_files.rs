use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use leafport_core::library::{LibraryError, Loan, Metadata, Settings, Source, Theme, paths};

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "leafport-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn sample_metadata() -> Metadata {
    Metadata {
        resource: "urn:uuid:7e28ffed-0000-4000-8000-000000000001".into(),
        title: "White Sand".into(),
        author: Some("Brandon Sanderson".into()),
        format: "application/pdf".into(),
        fulfillment: Some("ff-1".into()),
        source: Source::Cloud,
        synced_at: Some("2026-10-19T10:00:00Z".into()),
        excluded: false,
        returned: false,
        loan: None,
    }
}

#[test]
fn metadata_round_trips_with_and_without_loan() {
    let dir = tempdir();
    let mut m = sample_metadata();
    m.loan = Some(Loan {
        operator_url: "https://acs4.kobo.com/fulfillment".into(),
        expires: Some("2026-11-02T00:00:00Z".into()),
        token: "<loanToken>L</loanToken>".into(),
    });
    m.write(dir.path()).unwrap();
    assert_eq!(Metadata::read(dir.path()).unwrap(), m);
    m.loan = None;
    m.returned = true;
    m.write(dir.path()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("metadata.yaml")).unwrap();
    assert!(!text.contains("loan:"));
    assert_eq!(Metadata::read(dir.path()).unwrap(), m);
}

#[test]
fn metadata_yaml_uses_spec_keys() {
    let dir = tempdir();
    sample_metadata().write(dir.path()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("metadata.yaml")).unwrap();
    for key in [
        "resource: urn:uuid:",
        "title: White Sand",
        "source: cloud",
        "returned: false",
        "synced_at: ",
    ] {
        assert!(text.contains(key), "missing {key:?} in:\n{text}");
    }
}

#[test]
fn malformed_metadata_is_an_error() {
    let dir = tempdir();
    std::fs::write(dir.path().join("metadata.yaml"), "resource: [").unwrap();
    assert!(matches!(
        Metadata::read(dir.path()),
        Err(LibraryError::Yaml(_))
    ));
}

#[test]
fn settings_default_when_missing_and_round_trip() {
    let dir = tempdir();
    let path = dir.path().join("leafport/settings.yaml");
    assert_eq!(Settings::load(&path).unwrap(), Settings::default());
    let s = Settings {
        books_dir: Some("/books".into()),
        theme: Theme::Dark,
    };
    s.save(&path).unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("theme: dark")
    );
    assert_eq!(Settings::load(&path).unwrap(), s);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_dirs_follow_xdg_with_home_fallback() {
    let env = |k: &str| match k {
        "XDG_DATA_HOME" => Some("/x/data".into()),
        "HOME" => Some("/home/u".into()),
        _ => None,
    };
    assert_eq!(paths::data_dir_from(env), Some("/x/data/leafport".into()));
    let env = |k: &str| (k == "HOME").then(|| "/home/u".into());
    assert_eq!(
        paths::data_dir_from(env),
        Some("/home/u/.local/share/leafport".into())
    );
    assert_eq!(
        paths::config_dir_from(env),
        Some("/home/u/.config/leafport".into())
    );
}
