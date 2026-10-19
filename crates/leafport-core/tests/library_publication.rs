mod common;

use std::io::{Cursor, Read};

use common::{ADEPT_DATA, CHAPTER, CONTAINER, KEY, adept_epub, epub, protected_epub};
use leafport_core::library::{
    Format, LibraryError, decrypt_publication, epub_loan_token, extract_cover, pdfium_available,
};

const FONT: &[u8] = b"obfuscated font bytes";
const FONT_DATA: &str = r#"<enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
        <enc:CipherData><enc:CipherReference URI="OEBPS/font.otf"/></enc:CipherData>
    </enc:EncryptedData>"#;

fn read(zip: &mut zip::ZipArchive<Cursor<&Vec<u8>>>, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut out).unwrap();
    out
}

fn one_page_pdf() -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, b"0 0 m 10 10 l S".to_vec()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 300.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    document.save_to(&mut out).unwrap();
    out
}

fn encoded_image(format: image::ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::new_rgb8(4, 6)
        .write_to(&mut Cursor::new(&mut out), format)
        .unwrap();
    out
}

fn opf(metadata: &str, manifest: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>T</dc:title>{metadata}</metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    {manifest}
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#
    )
}

fn epub_with_opf(opf: &str, image: (&str, &[u8])) -> Vec<u8> {
    epub(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/ch1.xhtml", CHAPTER),
        image,
    ])
}

/// EPUB2 cover: `<meta name="cover">`, href with a percent-encoded space in a subdirectory.
fn epub_with_cover_meta(png: &[u8]) -> Vec<u8> {
    let opf = opf(
        r#"<meta name="cover" content="cover-img"/>"#,
        r#"<item id="cover-img" href="images/my%20cover.png" media-type="image/png"/>"#,
    );
    epub_with_opf(&opf, ("OEBPS/images/my cover.png", png))
}

/// EPUB3 cover: manifest item with `properties="cover-image"`, href with `..`.
fn epub_with_cover_image_property(jpeg: &[u8]) -> Vec<u8> {
    let opf = opf(
        "",
        r#"<item id="c" href="../art/cover.jpg" media-type="image/jpeg" properties="svg cover-image"/>"#,
    );
    epub_with_opf(&opf, ("art/cover.jpg", jpeg))
}

fn epub_without_cover() -> Vec<u8> {
    let png = encoded_image(image::ImageFormat::Png);
    let opf = opf(
        "",
        r#"<item id="img" href="images/a.png" media-type="image/png"/>"#,
    );
    epub_with_opf(&opf, ("OEBPS/images/a.png", &png))
}

#[test]
fn adept_epub_decrypts_to_a_plain_epub() {
    let out = decrypt_publication(&adept_epub(KEY), Format::Epub, None, common::key()).unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&out)).unwrap();
    {
        let first = zip.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), zip::CompressionMethod::Stored);
    }
    assert!(zip.by_name("META-INF/rights.xml").is_err());
    assert!(zip.by_name("META-INF/encryption.xml").is_err());
    assert_eq!(read(&mut zip, "OEBPS/ch1.xhtml"), CHAPTER);
    assert_eq!(
        zip.by_name("OEBPS/ch1.xhtml").unwrap().compression(),
        zip::CompressionMethod::Deflated
    );
    assert_eq!(read(&mut zip, "META-INF/container.xml"), CONTAINER);
}

#[test]
fn font_obfuscation_entries_survive_decryption() {
    let source = protected_epub(KEY, &[("OEBPS/font.otf", FONT)], &[ADEPT_DATA, FONT_DATA]);
    let out = decrypt_publication(&source, Format::Epub, None, common::key()).unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&out)).unwrap();
    let encryption = String::from_utf8(read(&mut zip, "META-INF/encryption.xml")).unwrap();
    assert!(encryption.contains("http://www.idpf.org/2008/embedding"));
    assert!(!encryption.contains("http://www.w3.org/2001/04/xmlenc#aes128-cbc"));
    assert!(roxmltree::Document::parse(&encryption).is_ok());
    assert_eq!(read(&mut zip, "OEBPS/font.otf"), FONT);
    assert_eq!(read(&mut zip, "OEBPS/ch1.xhtml"), CHAPTER);
}

#[test]
fn epub_for_another_device_is_a_drm_error() {
    // rights.xml is wrapped to another device.
    let result = decrypt_publication(&adept_epub(KEY), Format::Epub, None, common::other_key());
    assert!(matches!(result, Err(LibraryError::Drm(_))));
}

#[test]
fn plain_epub_and_plain_pdf_pass_through() {
    let e = epub(&[("mimetype", b"application/epub+zip")]);
    assert_eq!(
        decrypt_publication(&e, Format::Epub, None, common::key()).unwrap(),
        e
    );
    let p = one_page_pdf();
    assert_eq!(
        decrypt_publication(&p, Format::Pdf, None, common::key()).unwrap(),
        p
    );
}

#[test]
fn unknown_format_is_unsupported() {
    let result = decrypt_publication(b"data", Format::Unknown, None, common::key());
    assert!(matches!(result, Err(LibraryError::UnsupportedFormat)));
}

#[test]
fn loan_token_is_read_from_rights() {
    let rights = r#"<rights xmlns="http://ns.adobe.com/adept"><licenseToken/><loanToken><loan>L1</loan></loanToken></rights>"#;
    let book = epub(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/rights.xml", rights.as_bytes()),
    ]);
    let token = epub_loan_token(&book).unwrap();
    assert!(token.contains("loanToken"));
    assert!(token.contains("<loan>L1</loan>"));
    assert!(epub_loan_token(&adept_epub(KEY)).is_none());
    assert!(epub_loan_token(b"not a zip").is_none());
}

#[test]
fn epub_cover_comes_from_opf() {
    let png = encoded_image(image::ImageFormat::Png);
    let jpeg = encoded_image(image::ImageFormat::Jpeg);
    let c = extract_cover(&epub_with_cover_meta(&png), Format::Epub).unwrap();
    assert_eq!((c.extension, c.bytes), ("png", png));
    let c = extract_cover(&epub_with_cover_image_property(&jpeg), Format::Epub).unwrap();
    assert_eq!((c.extension, c.bytes), ("jpg", jpeg));
}

#[test]
fn epub_without_cover_has_none() {
    assert!(extract_cover(&epub_without_cover(), Format::Epub).is_none());
    assert!(extract_cover(b"not a zip", Format::Epub).is_none());
    assert!(extract_cover(b"data", Format::Unknown).is_none());
}

#[test]
fn epub_cover_with_other_media_type_has_none() {
    let opf = opf(
        r#"<meta name="cover" content="c"/>"#,
        r#"<item id="c" href="cover.gif" media-type="image/gif"/>"#,
    );
    let book = epub_with_opf(&opf, ("OEBPS/cover.gif", b"GIF89a"));
    assert!(extract_cover(&book, Format::Epub).is_none());
}

#[test]
fn pdf_cover_is_first_page_when_pdfium_loads() {
    let cover = extract_cover(&one_page_pdf(), Format::Pdf);
    if pdfium_available() {
        let c = cover.unwrap();
        assert_eq!(c.extension, "png");
        assert_eq!(image::load_from_memory(&c.bytes).unwrap().width(), 300);
    } else {
        assert!(cover.is_none());
    }
}
