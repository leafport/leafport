use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{
    Format, ItemState, Library, Loan, LocalItem, Metadata, Source, merge,
};

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
        "leafport-merge-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn record(acsm_xml: String) -> DownloadRecord {
    DownloadRecord {
        user: "u".into(),
        acsm: acsm_xml.into_bytes(),
    }
}

fn acsm(resource: &str, title: Option<&str>, src: Option<&str>) -> String {
    let title = title
        .map(|t| format!("<dc:title>{t}</dc:title>"))
        .unwrap_or_default();
    let src = src.map(|s| format!("<src>{s}</src>")).unwrap_or_default();
    format!(
        r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept"><resourceItemInfo><resource>{resource}</resource>{src}<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{title}<dc:format>application/epub+zip</dc:format></metadata></resourceItemInfo></fulfillmentToken>"#
    )
}

fn metadata(resource: &str, title: &str) -> Metadata {
    Metadata {
        resource: resource.into(),
        title: title.into(),
        author: None,
        format: "application/epub+zip".into(),
        fulfillment: None,
        source: Source::Cloud,
        synced_at: "2026-10-19T10:00:00Z".into(),
        returned: false,
        loan: None,
    }
}

fn local_with(metadata: Metadata) -> LocalItem {
    LocalItem {
        folder: PathBuf::from("/books").join(&metadata.title),
        metadata: Some(metadata),
        publication: None,
        cover: None,
        warning: None,
    }
}

fn local(resource: &str, title: &str) -> LocalItem {
    local_with(metadata(resource, title))
}

#[test]
fn merge_collapses_duplicate_cloud_records() {
    let a = record(acsm("urn:uuid:a", Some("Alpha"), None));
    assert_eq!(merge(&[a.clone(), a], &[]).len(), 1);
}

#[test]
fn merge_title_falls_back_from_metadata_to_acsm_to_src() {
    let rows = merge(
        &[record(acsm(
            "urn:uuid:a",
            Some("ACSM Title"),
            Some("https://x/F.epub"),
        ))],
        &[local("urn:uuid:a", "Meta Title")],
    );
    assert_eq!(rows[0].title, "Meta Title");
    assert_eq!(
        merge(&[record(acsm("urn:uuid:a", Some("ACSM Title"), None))], &[])[0].title,
        "ACSM Title"
    );
    assert_eq!(
        merge(
            &[record(acsm(
                "urn:uuid:a",
                None,
                Some("https://x/%D0%9C%D0%B8%D1%80.epub")
            ))],
            &[]
        )[0]
        .title,
        "Мир"
    );
    assert_eq!(
        merge(&[record(acsm("urn:uuid:a", None, None))], &[])[0].title,
        "urn:uuid:a"
    );
}

#[test]
fn merge_assigns_each_state() {
    let cloud: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .map(|k| {
            record(acsm(
                &format!("urn:uuid:{k}"),
                Some(&k.to_uppercase()),
                None,
            ))
        })
        .collect();
    let mut b = metadata("urn:uuid:b", "B");
    b.loan = Some(Loan {
        operator_url: "https://op".into(),
        expires: Some("2026-11-02T00:00:00Z".into()),
        token: "t".into(),
    });
    let mut c = metadata("urn:uuid:c", "C");
    c.returned = true;
    let locals = [
        local("urn:uuid:a", "A"),
        local_with(b),
        local_with(c),
        local("urn:uuid:e", "E"),
    ];
    let rows = merge(&cloud, &locals);
    let state = |t: &str| rows.iter().find(|r| r.title == t).unwrap().clone();
    assert_eq!(state("A").state, ItemState::Synced);
    assert_eq!(
        state("B").state,
        ItemState::OnLoan {
            expires: Some("2026-11-02T00:00:00Z".into())
        }
    );
    assert!(state("B").metadata.unwrap().loan.is_some());
    assert_eq!(state("C").state, ItemState::Returned);
    assert_eq!(state("D").state, ItemState::Available);
    assert!(state("D").acsm.is_some());
    assert_eq!(state("E").state, ItemState::Local);
    assert_eq!(state("E").key, "urn:uuid:e");
    assert_eq!(state("A").format, Format::Epub);
}

#[test]
fn merge_sorts_by_lowercase_title_then_key() {
    let rows = merge(
        &[
            record(acsm("urn:uuid:2", Some("beta"), None)),
            record(acsm("urn:uuid:1", Some("Alpha"), None)),
            record(acsm("urn:uuid:0", Some("Beta"), None)),
        ],
        &[],
    );
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["urn:uuid:1", "urn:uuid:0", "urn:uuid:2"]);
}

#[test]
fn merge_skips_unparseable_cloud_records() {
    let rows = merge(
        &[
            DownloadRecord {
                user: "u".into(),
                acsm: b"not xml".to_vec(),
            },
            record(acsm("urn:uuid:a", Some("Alpha"), None)),
        ],
        &[],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Alpha");
}

#[test]
fn open_reads_folders_sweeps_tmp_and_flags_malformed_metadata() {
    let dir = tempdir();
    let root = dir.path();
    std::fs::create_dir_all(root.join("Good")).unwrap();
    metadata("urn:uuid:g", "Good")
        .write(&root.join("Good"))
        .unwrap();
    std::fs::write(root.join("Good/Good.epub"), b"x").unwrap();
    std::fs::write(root.join("Good/cover.png"), b"x").unwrap();
    std::fs::create_dir_all(root.join("Bad")).unwrap();
    std::fs::write(root.join("Bad/metadata.yaml"), "resource: [").unwrap();
    std::fs::create_dir_all(root.join(".tmp-x")).unwrap();
    std::fs::create_dir_all(root.join("NoMeta")).unwrap();
    std::fs::write(root.join("stray.txt"), b"x").unwrap();

    let lib = Library::open(root).unwrap();
    assert!(!root.join(".tmp-x").exists());
    assert_eq!(lib.local.len(), 2);
    let bad = &lib.local[0];
    let good = &lib.local[1];
    assert!(bad.metadata.is_none());
    assert!(
        bad.warning
            .as_deref()
            .unwrap()
            .starts_with("metadata.yaml is malformed")
    );
    assert_eq!(
        good.publication.as_deref(),
        Some(root.join("Good/Good.epub").as_path())
    );
    assert_eq!(
        good.cover.as_deref(),
        Some(root.join("Good/cover.png").as_path())
    );

    let rows = merge(&[], &lib.local);
    let row = rows.iter().find(|r| r.key == "local:Bad").unwrap();
    assert_eq!(row.title, "Bad");
    assert_eq!(row.state, ItemState::Local);
    assert!(row.warning.is_some());
}

#[test]
fn open_creates_missing_root() {
    let dir = tempdir();
    let root = dir.path().join("Books");
    assert!(Library::open(&root).unwrap().local.is_empty());
    assert!(root.is_dir());
}
