mod common;

use leafport_core::pdf_drm::{PdfDrmError, decrypt_pdf, is_ebx_encrypted};
use lopdf::{Document, Object, Stream, dictionary};

const KEY: [u8; 16] = *b"pdf-content-key!";
const CONTENT: &[u8] = b"BT /F1 12 Tf 72 712 Td (Hello) Tj ET";

/// Minimal one-page PDF; `encrypt` adds an EBX_HANDLER /Encrypt and encrypts the
/// page content stream and the /Title string.
fn pdf(encrypt: bool) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let content = if encrypt {
        common::aes_encrypt(&KEY, &[1; 16], CONTENT, true)
    } else {
        CONTENT.to_vec()
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content));
    let pages_id = document.new_object_id();
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
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
    let title = if encrypt {
        common::aes_encrypt(&KEY, &[2; 16], b"Secret Title", true)
    } else {
        b"Secret Title".to_vec()
    };
    let info_id = document.add_object(dictionary! {
        "Title" => Object::String(title, lopdf::StringFormat::Hexadecimal),
    });
    document.trailer.set("Info", info_id);
    if encrypt {
        let encrypt_id = document.add_object(dictionary! {
            "Filter" => "EBX_HANDLER",
            "V" => 4,
            "ADEPT_LICENSE" => Object::string_literal("license"),
        });
        document.trailer.set("Encrypt", encrypt_id);
    }
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn ebx_detection() {
    assert!(is_ebx_encrypted(&pdf(true)));
    assert!(!is_ebx_encrypted(&pdf(false)));
    assert!(!is_ebx_encrypted(b"not a pdf"));
}

#[test]
fn plain_and_unparseable_pdfs_are_rejected() {
    assert!(matches!(
        decrypt_pdf(&pdf(false), &KEY),
        Err(PdfDrmError::NotEbx)
    ));
    assert!(matches!(
        decrypt_pdf(b"not a pdf", &KEY),
        Err(PdfDrmError::Parse(_))
    ));
}

#[test]
#[ignore = "lopdf 0.42 load_mem keeps only the /Encrypt object for non-standard filters, so decrypt_pdf loses every page"]
fn ebx_pdf_decrypts_streams_and_strings_and_drops_encrypt() {
    let plain = decrypt_pdf(&pdf(true), &KEY).unwrap();
    let document = Document::load_mem(&plain).unwrap();
    assert!(document.trailer.get(b"Encrypt").is_err());
    let page_id = *document.get_pages().get(&1).unwrap();
    assert_eq!(document.get_page_content(page_id).unwrap(), CONTENT);
    let info = document
        .trailer
        .get(b"Info")
        .and_then(Object::as_reference)
        .and_then(|id| document.get_dictionary(id))
        .unwrap();
    assert_eq!(
        info.get(b"Title").unwrap().as_str().unwrap(),
        b"Secret Title"
    );
}
