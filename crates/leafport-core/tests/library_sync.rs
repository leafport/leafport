mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{
    ADEPT_DATA, FakeTransport, IV, KEY, acsm_xml, activation, adept_epub, aes_encrypt,
    fulfillment_xml, protected_epub, protected_epub_with,
};
use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{
    ItemState, Library, LibraryError, Metadata, Source, load_cache, merge, refresh, return_loan,
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
        "leafport-sync-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// No `.tmp-` temp or `.old-` set-aside entry remains in `root`.
fn no_tmp_left(root: &Path) -> bool {
    std::fs::read_dir(root).unwrap().all(|e| {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        !name.starts_with(".tmp-") && !name.starts_with(".old-")
    })
}

/// An ADEPT EPUB whose chapter ciphertext is cut to 20 bytes, so decryption fails.
fn truncated_epub() -> Vec<u8> {
    let chapter = aes_encrypt(&KEY, &IV, common::CHAPTER, true);
    protected_epub_with(&chapter[..20], &[], &[ADEPT_DATA])
}

fn epub_with_text(text: &[u8]) -> Vec<u8> {
    protected_epub_with(&aes_encrypt(&KEY, &IV, text, true), &[], &[ADEPT_DATA])
}

fn record(acsm: String) -> DownloadRecord {
    DownloadRecord {
        user: "urn:uuid:user".into(),
        acsm: acsm.into_bytes(),
    }
}

fn sync(lib: &Library, resource: &str, title: &str, loan: Option<&str>, book: Vec<u8>) {
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(resource, title, "https://dl.example/a.epub", loan),
        )
        .respond("/a.epub", 200, book);
    lib.sync_item(acsm_xml(resource, title).as_bytes(), &activation(), &t)
        .unwrap();
}

#[test]
fn sync_item_writes_title_folder_with_decrypted_epub_acsm_and_metadata() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, adept_epub(KEY));
    let out = lib
        .sync_item(
            acsm_xml("urn:uuid:a", "White Sand").as_bytes(),
            &activation(),
            &t,
        )
        .unwrap();
    let folder = root.path().join("White Sand [a]");
    assert_eq!(out.folder, folder);
    assert!(folder.join("White Sand.epub").is_file() && folder.join("White Sand.acsm").is_file());
    assert_eq!(
        std::fs::read(folder.join("White Sand.acsm")).unwrap(),
        acsm_xml("urn:uuid:a", "White Sand").as_bytes()
    );
    let metadata = Metadata::read(&folder).unwrap();
    assert_eq!(metadata.resource, "urn:uuid:a");
    assert_eq!(metadata.title, "White Sand");
    assert_eq!(metadata.format, "application/epub+zip");
    assert_eq!(metadata.fulfillment.as_deref(), Some("F-1"));
    assert_eq!(metadata.source, Source::Cloud);
    assert!(!metadata.returned && metadata.loan.is_none());
    assert_eq!(out.metadata, metadata);
    assert!(out.warning.is_none());
    // The written publication is decrypted: no ADEPT rights remain.
    let written = std::fs::read(folder.join("White Sand.epub")).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&written)).unwrap();
    assert!(zip.by_name("META-INF/rights.xml").is_err());
    assert!(no_tmp_left(root.path()));
    let rows = merge(&[], &Library::open(root.path()).unwrap().local);
    assert_eq!(rows[0].state, ItemState::Local); // no cloud record in this merge
}

#[test]
fn sync_item_records_loan_block_and_expiry() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    sync(
        &lib,
        "urn:uuid:a",
        "White Sand",
        Some("2026-11-02T00:00:00Z"),
        adept_epub(KEY),
    );
    let loan = Metadata::read(&root.path().join("White Sand [a]"))
        .unwrap()
        .loan
        .unwrap();
    assert_eq!(loan.operator_url, "https://op.example/fulfillment");
    assert_eq!(loan.expires.as_deref(), Some("2026-11-02T00:00:00Z"));
    assert!(loan.token.contains("loanToken"));
}

const UUID: &str = "urn:uuid:7e28ffed-5120-2115-d314-b3975b1b6481";

/// A title folder `name` in `root` that belongs to `resource`.
fn foreign_folder(root: &Path, name: &str, resource: &str) -> PathBuf {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("keep.txt"), "other").unwrap();
    Metadata {
        resource: resource.into(),
        title: "White Sand".into(),
        author: None,
        format: "application/epub+zip".into(),
        fulfillment: None,
        source: Source::Cloud,
        synced_at: "2026-10-19T10:00:00Z".into(),
        returned: false,
        loan: None,
    }
    .write(&folder)
    .unwrap();
    folder
}

#[test]
fn same_title_for_different_resources_gets_distinct_suffixed_folders() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    sync(&lib, UUID, "Same", None, adept_epub(KEY));
    sync(
        &lib,
        "urn:uuid:11112222-0000-0000-0000-000000000000",
        "Same",
        None,
        adept_epub(KEY),
    );
    let first = root.path().join("Same [7e28ffed]");
    let second = root.path().join("Same [11112222]");
    assert!(first.join("Same.epub").is_file() && second.join("Same.epub").is_file());
    assert_eq!(Metadata::read(&first).unwrap().resource, UUID);
    assert_eq!(
        Metadata::read(&second).unwrap().resource,
        "urn:uuid:11112222-0000-0000-0000-000000000000"
    );
}

#[test]
fn folder_title_comes_from_fulfillment_not_acsm() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, adept_epub(KEY));
    let out = lib
        .sync_item(
            acsm_xml("urn:uuid:a", "Shop Listing Name").as_bytes(),
            &activation(),
            &t,
        )
        .unwrap();
    assert_eq!(out.folder, root.path().join("White Sand [a]"));
    assert!(out.folder.join("White Sand.epub").is_file());
    assert_eq!(out.metadata.title, "White Sand");
}

#[test]
fn folder_title_comes_from_book_metadata() {
    let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Book Title</dc:title></metadata>
  <manifest/><spine/>
</package>"#;
    let book = protected_epub(KEY, &[("OEBPS/content.opf", opf)], &[ADEPT_DATA]);
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "Server Title",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, book);
    let out = lib
        .sync_item(
            acsm_xml("urn:uuid:a", "ACSM Title").as_bytes(),
            &activation(),
            &t,
        )
        .unwrap();
    assert_eq!(out.folder, root.path().join("Book Title [a]"));
    assert!(out.folder.join("Book Title.epub").is_file());
    assert!(out.folder.join("Book Title.acsm").is_file());
    assert_eq!(out.metadata.title, "Book Title");
}

#[test]
fn resync_on_same_library_instance_reuses_folder() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    sync(&lib, "urn:uuid:a", "White Sand", None, adept_epub(KEY));
    sync(&lib, "urn:uuid:a", "White Sand", None, adept_epub(KEY));
    let names: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["White Sand [a]"]);
}

#[test]
fn name_taken_by_other_resource_uses_full_id() {
    let root = tempdir();
    let other = foreign_folder(
        root.path(),
        "White Sand [7e28ffed]",
        "urn:uuid:7e28ffed-0000-0000-0000-000000000000",
    );
    let lib = Library::open(root.path()).unwrap();
    sync(&lib, UUID, "White Sand", None, adept_epub(KEY));
    let full = root
        .path()
        .join("White Sand [7e28ffed-5120-2115-d314-b3975b1b6481]");
    assert_eq!(Metadata::read(&full).unwrap().resource, UUID);
    assert!(full.join("White Sand.epub").is_file());
    assert_eq!(
        Metadata::read(&other).unwrap().resource,
        "urn:uuid:7e28ffed-0000-0000-0000-000000000000"
    );
    assert!(other.join("keep.txt").is_file() && !other.join("White Sand.epub").exists());
}

#[test]
fn full_id_name_taken_by_other_resource_fails_without_replacing() {
    let root = tempdir();
    foreign_folder(root.path(), "White Sand [7e28ffed]", "urn:uuid:x");
    let full = foreign_folder(
        root.path(),
        "White Sand [7e28ffed-5120-2115-d314-b3975b1b6481]",
        "urn:uuid:y",
    );
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(UUID, "White Sand", "https://dl.example/a.epub", None),
        )
        .respond("/a.epub", 200, adept_epub(KEY));
    let result = lib.sync_item(acsm_xml(UUID, "White Sand").as_bytes(), &activation(), &t);
    assert!(matches!(result, Err(LibraryError::Io(_))), "{result:?}");
    assert_eq!(Metadata::read(&full).unwrap().resource, "urn:uuid:y");
    assert!(full.join("keep.txt").is_file());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn adept_error_response_leaves_nothing_and_reports_data() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let acsm = acsm_xml("urn:uuid:a", "White Sand");
    let t = FakeTransport::new().respond(
        "/Fulfill",
        403,
        r#"<error xmlns="http://ns.adobe.com/adept" data="E_LIC_ALREADY_FULFILLED_BY_ANOTHER_USER"/>"#,
    );
    let err = lib
        .sync_item(acsm.as_bytes(), &activation(), &t)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("E_LIC_ALREADY_FULFILLED_BY_ANOTHER_USER")
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn drm_failure_leaves_nothing() {
    let root = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, truncated_epub());
    let result = lib.sync_item(
        acsm_xml("urn:uuid:a", "White Sand").as_bytes(),
        &activation(),
        &t,
    );
    assert!(matches!(result, Err(LibraryError::Drm(_))), "{result:?}");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn failed_resync_keeps_previous_folder() {
    let root = tempdir();
    sync(
        &Library::open(root.path()).unwrap(),
        "urn:uuid:a",
        "White Sand",
        None,
        adept_epub(KEY),
    );
    let book = root.path().join("White Sand [a]").join("White Sand.epub");
    let first = std::fs::read(&book).unwrap();
    let lib = Library::open(root.path()).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, truncated_epub());
    let result = lib.sync_item(
        acsm_xml("urn:uuid:a", "White Sand").as_bytes(),
        &activation(),
        &t,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&book).unwrap(), first);
    assert!(no_tmp_left(root.path()));
}

#[test]
fn resync_replaces_existing_folder() {
    let root = tempdir();
    sync(
        &Library::open(root.path()).unwrap(),
        "urn:uuid:a",
        "White Sand",
        None,
        adept_epub(KEY),
    );
    let lib = Library::open(root.path()).unwrap();
    sync(
        &lib,
        "urn:uuid:a",
        "White Sand",
        None,
        epub_with_text(b"<p>Second edition</p>"),
    );
    let written =
        std::fs::read(root.path().join("White Sand [a]").join("White Sand.epub")).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&written)).unwrap();
    let mut chapter = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("OEBPS/ch1.xhtml").unwrap(), &mut chapter)
        .unwrap();
    assert_eq!(chapter, "<p>Second edition</p>");
    assert!(no_tmp_left(root.path()));
    assert_eq!(Library::open(root.path()).unwrap().local.len(), 1);
}

#[test]
fn import_acsm_uploads_then_syncs() {
    let root = tempdir();
    let input = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let path = input.path().join("book.acsm");
    std::fs::write(&path, acsm_xml("urn:uuid:a", "White Sand")).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/ebookcloud/SyncToCloud",
            200,
            r#"<synctocloudResult xmlns="http://ns.adobe.com/adept">SUCCESS</synctocloudResult>"#,
        )
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, adept_epub(KEY));
    let out = lib.import_acsm(&path, &activation(), &t).unwrap();
    assert!(out.warning.is_none());
    assert_eq!(out.metadata.source, Source::Import);
    assert_eq!(Metadata::read(&out.folder).unwrap().source, Source::Import);
    assert!(t.requests.lock().unwrap()[0].0.ends_with("/SyncToCloud"));
}

#[test]
fn import_acsm_upload_failure_still_syncs_with_warning() {
    let root = tempdir();
    let input = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let path = input.path().join("book.acsm");
    std::fs::write(&path, acsm_xml("urn:uuid:a", "White Sand")).unwrap();
    let t = FakeTransport::new()
        .respond(
            "/ebookcloud/SyncToCloud",
            500,
            r#"<error xmlns="http://ns.adobe.com/adept" data="E_UP"/>"#,
        )
        .respond(
            "/fulfillment/Fulfill",
            200,
            fulfillment_xml(
                "urn:uuid:a",
                "White Sand",
                "https://dl.example/a.epub",
                None,
            ),
        )
        .respond("/a.epub", 200, adept_epub(KEY));
    let out = lib.import_acsm(&path, &activation(), &t).unwrap();
    let warning = out.warning.unwrap();
    assert!(warning.contains("Upload to cloud failed") && warning.contains("E_UP"));
    assert!(out.folder.join("White Sand.epub").is_file());
}

#[test]
fn import_acsm_rejects_invalid_acsm_without_upload() {
    let root = tempdir();
    let input = tempdir();
    let lib = Library::open(root.path()).unwrap();
    let path = input.path().join("bad.acsm");
    std::fs::write(
        &path,
        "<fulfillmentToken xmlns=\"http://ns.adobe.com/adept\"/>",
    )
    .unwrap();
    let t = FakeTransport::new();
    assert!(lib.import_acsm(&path, &activation(), &t).is_err());
    assert!(t.requests.lock().unwrap().is_empty());
}

fn loan_item(root: &Path) -> leafport_core::library::Item {
    sync(
        &Library::open(root).unwrap(),
        "urn:uuid:a",
        "White Sand",
        Some("2026-11-02T00:00:00Z"),
        adept_epub(KEY),
    );
    let rows = merge(
        &[record(acsm_xml("urn:uuid:a", "White Sand"))],
        &Library::open(root).unwrap().local,
    );
    assert!(matches!(rows[0].state, ItemState::OnLoan { .. }));
    rows.into_iter().next().unwrap()
}

#[test]
fn return_loan_clears_loan_and_marks_returned() {
    let root = tempdir();
    let item = loan_item(root.path());
    let t = FakeTransport::new().respond(
        "/fulfillment/LoanReturn",
        200,
        "<envelope xmlns=\"http://ns.adobe.com/adept\"/>",
    );
    let metadata = return_loan(&item, &activation(), &t).unwrap();
    assert!(metadata.loan.is_none() && metadata.returned);
    let folder = root.path().join("White Sand [a]");
    assert_eq!(Metadata::read(&folder).unwrap(), metadata);
    assert!(folder.join("White Sand.epub").is_file());
    let rows = merge(
        &[record(acsm_xml("urn:uuid:a", "White Sand"))],
        &Library::open(root.path()).unwrap().local,
    );
    assert_eq!(rows[0].state, ItemState::Returned);
}

#[test]
fn return_loan_error_keeps_loan() {
    let root = tempdir();
    let item = loan_item(root.path());
    let t = FakeTransport::new().respond(
        "/fulfillment/LoanReturn",
        200,
        r#"<error xmlns="http://ns.adobe.com/adept" data="E_LOAN_NOT_ON_RECORD"/>"#,
    );
    let err = return_loan(&item, &activation(), &t).unwrap_err();
    assert!(err.to_string().contains("E_LOAN_NOT_ON_RECORD"));
    let metadata = Metadata::read(&root.path().join("White Sand [a]")).unwrap();
    assert!(metadata.loan.is_some() && !metadata.returned);
}

#[test]
fn return_loan_without_loan_is_not_on_loan() {
    let root = tempdir();
    sync(
        &Library::open(root.path()).unwrap(),
        "urn:uuid:a",
        "White Sand",
        None,
        adept_epub(KEY),
    );
    let rows = merge(&[], &Library::open(root.path()).unwrap().local);
    let err = return_loan(&rows[0], &activation(), &FakeTransport::new()).unwrap_err();
    assert!(matches!(err, LibraryError::NotOnLoan));
}

#[test]
fn refresh_writes_cache_verbatim_and_failure_keeps_it() {
    let root = tempdir();
    let cache = root.path().join("data").join("cloud-cache.xml");
    let body = format!(
        r#"<synctodeviceResult xmlns="http://ns.adobe.com/adept"><user2acsm><user>urn:uuid:user</user><acsm>{}</acsm></user2acsm></synctodeviceResult>"#,
        STANDARD.encode(acsm_xml("urn:uuid:a", "White Sand"))
    );
    let t = FakeTransport::new().respond("/ebookcloud/SyncToDevice", 200, body.clone());
    let records = refresh(&cache, &activation(), &t).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), body);
    assert_eq!(load_cache(&cache).unwrap().len(), 1);
    let t = FakeTransport::new().respond("/ebookcloud/SyncToDevice", 500, "oops");
    assert!(refresh(&cache, &activation(), &t).is_err());
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), body);
    assert_eq!(
        std::fs::read_dir(cache.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn cache_missing_is_empty_and_corrupt_is_error() {
    let root = tempdir();
    assert!(
        load_cache(&root.path().join("none.xml"))
            .unwrap()
            .is_empty()
    );
    std::fs::write(root.path().join("c.xml"), "<not").unwrap();
    assert!(load_cache(&root.path().join("c.xml")).is_err());
}
