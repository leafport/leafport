mod common;

use leafport_core::pdf_drm::{PdfDrmError, decrypt_pdf, is_ebx_encrypted};
use lopdf::{Document, Object, SaveOptions, Stream, dictionary};

const KEY: [u8; 16] = *b"pdf-content-key!";
const CONTENT: &[u8] = b"BT /F1 12 Tf 72 712 Td (Hello) Tj ET";

/// Minimal one-page PDF; `encrypt` adds an EBX_HANDLER /Encrypt and encrypts the
/// page content stream and the /Title string.
fn pdf(encrypt: bool) -> Vec<u8> {
    pdf_with(encrypt, false)
}

/// As [`pdf`], optionally written with a cross-reference stream, so the trailer
/// (and its /Encrypt key) lives in a stream dictionary.
fn pdf_with(encrypt: bool, xref_stream: bool) -> Vec<u8> {
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
    if xref_stream {
        let options = SaveOptions::builder().use_xref_streams(true).build();
        document.save_with_options(&mut bytes, options).unwrap();
    } else {
        document.save_to(&mut bytes).unwrap();
    }
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
fn ebx_pdf_decrypts_streams_and_strings_and_drops_encrypt() {
    assert_decrypts(&pdf(true));
}

#[test]
fn ebx_pdf_with_xref_stream_decrypts() {
    let encrypted = pdf_with(true, true);
    // No classic trailer: /Encrypt sits in the xref stream dictionary.
    assert!(!encrypted.windows(7).any(|w| w == b"trailer"));
    assert!(is_ebx_encrypted(&encrypted));
    assert_decrypts(&encrypted);
}

fn assert_decrypts(encrypted: &[u8]) {
    let plain = decrypt_pdf(encrypted, &KEY).unwrap();
    let document = Document::load_mem(&plain).unwrap();
    assert!(document.trailer.get(b"Encrypt").is_err());
    let page_id = *document.get_pages().get(&1).unwrap();
    // get_page_content re-encodes the operations, which appends a newline.
    assert_eq!(
        document.get_page_content(page_id).unwrap().trim_ascii_end(),
        CONTENT
    );
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
