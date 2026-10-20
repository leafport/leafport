use leafport_core::library::{
    Format, LibraryError, acsm_summary, folder_name, full_id_suffix, id_suffix, src_title,
};

const SAMPLE_ACSM: &str = r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><distributor>urn:uuid:d</distributor><operatorURL>https://acs4.kobo.com/fulfillment</operatorURL><transaction>t</transaction><expiration>2020-01-01T00:00:00Z</expiration><resourceItemInfo><resource>urn:uuid:7e28ffed-5120-2115-d314-b3975b1b6481</resource><resourceItem>0</resourceItem><metadata><dc:title>White Sand</dc:title><dc:creator>Brandon Sanderson</dc:creator><dc:format>application/pdf</dc:format></metadata><src>https://acs4.kobo.com/dl/White%20Sand.pdf</src></resourceItemInfo></fulfillmentToken>"#;

const NO_RESOURCE_ACSM: &str = r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><operatorURL>https://acs4.kobo.com/fulfillment</operatorURL><resourceItemInfo><metadata><dc:title>No Id</dc:title></metadata></resourceItemInfo></fulfillmentToken>"#;

#[test]
fn folder_name_strips_ade_invalid_characters_and_collapses_whitespace() {
    assert_eq!(
        folder_name("Star Wars: A New Hope?"),
        "Star Wars A New Hope"
    );
    assert_eq!(folder_name("a/b\\c|d*e\"f<g>h"), "abcdefgh");
    assert_eq!(folder_name("  Many \t\n  spaces  "), "Many spaces");
}

#[test]
fn folder_name_keeps_unicode_scripts() {
    assert_eq!(folder_name("Мастер и Маргарита"), "Мастер и Маргарита");
    assert_eq!(folder_name("ノルウェイの森"), "ノルウェイの森");
}

#[test]
fn folder_name_handles_hostile_titles() {
    assert_eq!(folder_name("A\tB"), "A B");
    assert_eq!(folder_name("Line1\nLine2"), "Line1 Line2");
    assert_eq!(folder_name("???"), "Untitled");
    assert_eq!(folder_name("con"), "con_");
    assert_eq!(folder_name("COM3.txt"), "COM3_.txt");
    assert_eq!(folder_name("Com10"), "Com10");
    assert_eq!(folder_name(".hack//Sign"), "hackSign");
    assert_eq!(folder_name("Ends with dot."), "Ends with dot");
    let long = folder_name(&"é".repeat(400));
    assert!(long.len() <= 200 && long.chars().all(|c| c == 'é'));
}

#[test]
fn id_suffix_uses_first_eight_id_characters() {
    assert_eq!(
        id_suffix("urn:uuid:7e28ffed-5120-2115-d314-b3975b1b6481"),
        " [7e28ffed]"
    );
    assert_eq!(
        id_suffix("sha1:0123456789abcdef0123456789abcdef01234567"),
        " [01234567]"
    );
    assert_eq!(id_suffix("isbn:12345678901"), " [isbn123]");
}

#[test]
fn full_id_suffix_keeps_whole_id() {
    assert_eq!(
        full_id_suffix("urn:uuid:7e28ffed-5120-2115-d314-b3975b1b6481"),
        " [7e28ffed-5120-2115-d314-b3975b1b6481]"
    );
    assert_eq!(
        full_id_suffix("sha1:0123456789abcdef0123456789abcdef01234567"),
        " [0123456789abcdef0123456789abcdef01234567]"
    );
    assert_eq!(full_id_suffix("isbn:1/2"), " [isbn12]");
}

#[test]
fn acsm_summary_reads_resource_title_author_format_and_src() {
    let s = acsm_summary(SAMPLE_ACSM.as_bytes()).unwrap();
    assert_eq!(s.key, "urn:uuid:7e28ffed-5120-2115-d314-b3975b1b6481");
    assert_eq!(s.title.as_deref(), Some("White Sand"));
    assert_eq!(s.author.as_deref(), Some("Brandon Sanderson"));
    assert_eq!(s.format, Format::Pdf);
    assert_eq!(
        s.src.as_deref(),
        Some("https://acs4.kobo.com/dl/White%20Sand.pdf")
    );
}

#[test]
fn acsm_without_resource_is_keyed_by_hash() {
    let s = acsm_summary(NO_RESOURCE_ACSM.as_bytes()).unwrap();
    assert!(s.key.starts_with("sha1:") && s.key.len() == 45);
    assert_eq!(
        acsm_summary(NO_RESOURCE_ACSM.as_bytes()).unwrap().key,
        s.key
    );
}

#[test]
fn acsm_summary_rejects_bad_xml() {
    assert!(matches!(acsm_summary(b"<a>"), Err(LibraryError::Acsm(_))));
}

#[test]
fn format_helpers() {
    assert_eq!(Format::sniff(b"%PDF-1.7"), Format::Pdf);
    assert_eq!(Format::sniff(b"PK\x03\x04x"), Format::Epub);
    assert_eq!(
        Format::from_mime("application/epub+zip").extension(),
        Some("epub")
    );
    assert_eq!(Format::Unknown.label(), "?");
}

#[test]
fn src_title_is_url_decoded_including_cyrillic() {
    assert_eq!(
        src_title("https://x/dl/White%20Sand.epub?t=1").as_deref(),
        Some("White Sand")
    );
    assert_eq!(
        src_title("https://x/%D0%9C%D0%B0%D1%81%D1%82%D0%B5%D1%80.pdf").as_deref(),
        Some("Мастер")
    );
}

#[test]
fn acsm_summary_falls_back_to_document_wide_dc_lookup() {
    let xml = r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><resourceItemInfo><metadata><dc:creator>Au</dc:creator></metadata><dc:title>Elsewhere</dc:title></resourceItemInfo></fulfillmentToken>"#;
    let s = acsm_summary(xml.as_bytes()).unwrap();
    assert_eq!(s.title.as_deref(), Some("Elsewhere"));
    assert_eq!(s.author.as_deref(), Some("Au"));
}
