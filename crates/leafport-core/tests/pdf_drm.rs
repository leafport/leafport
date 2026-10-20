mod common;

use std::io::Write;

use base64::{Engine, engine::general_purpose::STANDARD};
use leafport_core::pdf_drm::{PdfDrmError, content_key, decrypt_pdf, is_ebx_encrypted};
use leafport_core::rsa::Pkcs1v15Encrypt;
use lopdf::{
    Dictionary, Document, Object, ObjectId, SaveOptions, Stream, StringFormat, dictionary,
};

const KEY: [u8; 16] = *b"pdf-content-key!";
const CONTENT: &[u8] = b"BT /F1 12 Tf 72 712 Td (Hello) Tj ET";
const TITLE: &[u8] = b"Secret Title";
const NOTE: &[u8] = b"stream dict string";
const XMP: &[u8] = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cipher {
    Aes,
    Rc4,
    Identity,
}

/// How a fixture is encrypted: the /Encrypt entries besides /Filter and /ADEPT_LICENSE,
/// and the ciphers they select for streams and strings.
struct Scheme {
    entries: Dictionary,
    stream: Cipher,
    string: Cipher,
    encrypt_metadata: bool,
}

/// The default scheme for `cipher`: V 4 with an AESV2 crypt filter, or V 2 RC4.
fn scheme(cipher: Cipher) -> Scheme {
    let entries = match cipher {
        Cipher::Aes => v4_entries(
            dictionary! { "StdCF" => dictionary! { "CFM" => "AESV2" } },
            "StdCF",
            "StdCF",
        ),
        _ => dictionary! { "V" => 2 },
    };
    Scheme {
        entries,
        stream: cipher,
        string: cipher,
        encrypt_metadata: true,
    }
}

fn v4_entries(filters: Dictionary, stream_filter: &str, string_filter: &str) -> Dictionary {
    dictionary! {
        "V" => 4,
        "CF" => filters,
        "StmF" => Object::Name(stream_filter.as_bytes().to_vec()),
        "StrF" => Object::Name(string_filter.as_bytes().to_vec()),
    }
}

/// Minimal one-page PDF; `cipher` adds an EBX_HANDLER /Encrypt (V 4 for AES, V 2 for
/// RC4) and encrypts the page content stream and the /Title string with `KEY`.
fn pdf(cipher: Option<Cipher>) -> Vec<u8> {
    pdf_with(cipher, &KEY, false)
}

/// As [`pdf`] with an explicit book key, optionally written with a cross-reference
/// stream, so the trailer (and its /Encrypt key) lives in a stream dictionary.
fn pdf_with(cipher: Option<Cipher>, book_key: &[u8], xref_stream: bool) -> Vec<u8> {
    let (mut document, _) = plain_document();
    if let Some(cipher) = cipher {
        encrypt(&mut document, cipher, book_key);
    }
    save(document, xref_stream)
}

/// The plain document and its /Info object id.
fn plain_document() -> (Document, ObjectId) {
    plain_document_at((1, 0), (2, 0))
}

/// Plain one-page document whose FlateDecode content stream (with a /Note string in its
/// dictionary) and /Info dictionary get the given object ids.
fn plain_document_at(content_id: ObjectId, info_id: ObjectId) -> (Document, ObjectId) {
    let mut document = Document::with_version("1.5");
    document.max_id = content_id.0.max(info_id.0);
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(CONTENT).unwrap();
    document.objects.insert(
        content_id,
        Object::Stream(Stream::new(
            dictionary! {
                "Filter" => "FlateDecode",
                "Note" => Object::string_literal(NOTE),
            },
            zlib.finish().unwrap(),
        )),
    );
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
    document.objects.insert(
        info_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::String(TITLE.to_vec(), StringFormat::Hexadecimal),
        }),
    );
    document.trailer.set("Info", info_id);
    (document, info_id)
}

/// Encrypt under the default [`scheme`] for `cipher`.
fn encrypt(document: &mut Document, cipher: Cipher, book_key: &[u8]) {
    encrypt_with(document, &scheme(cipher), book_key);
}

/// Encrypt every string and stream with its per-object key under `scheme`, then add the
/// EBX /Encrypt dictionary carrying a real /ADEPT_LICENSE for `book_key`.
fn encrypt_with(document: &mut Document, scheme: &Scheme, book_key: &[u8]) {
    for (id, object) in document.objects.iter_mut() {
        let keys = Keys {
            stream: object_key(book_key, *id, scheme.stream),
            string: object_key(book_key, *id, scheme.string),
            iv_byte: id.0 as u8,
        };
        encrypt_object(object, &keys, scheme);
    }
    let mut encrypt = scheme.entries.clone();
    encrypt.set("Filter", "EBX_HANDLER");
    encrypt.set(
        "ADEPT_LICENSE",
        Object::string_literal(adept_license(book_key)),
    );
    let encrypt_id = document.add_object(encrypt);
    document.trailer.set("Encrypt", encrypt_id);
}

struct Keys {
    stream: Vec<u8>,
    string: Vec<u8>,
    iv_byte: u8,
}

fn save(mut document: Document, xref_stream: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if xref_stream {
        let options = SaveOptions::builder().use_xref_streams(true).build();
        document.save_with_options(&mut bytes, options).unwrap();
    } else {
        document.save_to(&mut bytes).unwrap();
    }
    bytes
}

fn encrypt_object(object: &mut Object, keys: &Keys, scheme: &Scheme) {
    match object {
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter_mut() {
                encrypt_object(value, keys, scheme);
            }
            if scheme.encrypt_metadata || !stream.dict.has_type(b"Metadata") {
                let encrypted =
                    encrypt_value(&stream.content, &keys.stream, scheme.stream, keys.iv_byte);
                stream.set_content(encrypted);
            }
        }
        Object::String(bytes, _) => {
            *bytes = encrypt_value(bytes, &keys.string, scheme.string, keys.iv_byte)
        }
        Object::Array(items) => {
            for item in items {
                encrypt_object(item, keys, scheme);
            }
        }
        Object::Dictionary(dict) => {
            for (_, value) in dict.iter_mut() {
                encrypt_object(value, keys, scheme);
            }
        }
        _ => {}
    }
}

fn encrypt_value(plain: &[u8], key: &[u8], cipher: Cipher, iv_byte: u8) -> Vec<u8> {
    match cipher {
        Cipher::Aes => common::aes_encrypt(key.try_into().unwrap(), &[iv_byte; 16], plain, true),
        Cipher::Rc4 => rc4(key, plain),
        Cipher::Identity => plain.to_vec(),
    }
}

/// MD5(book key ‖ object number 3B LE ‖ generation 2B LE ‖ "sAlT" for AES), truncated
/// to min(book key length + 5, 16).
fn object_key(book_key: &[u8], (number, generation): ObjectId, cipher: Cipher) -> Vec<u8> {
    use md5::{Digest, Md5};
    let mut input = book_key.to_vec();
    input.extend_from_slice(&number.to_le_bytes()[..3]);
    input.extend_from_slice(&generation.to_le_bytes());
    if cipher == Cipher::Aes {
        input.extend_from_slice(b"sAlT");
    }
    Md5::digest(&input)[..(book_key.len() + 5).min(16)].to_vec()
}

/// Independent textbook RC4 (KSA + PRGA), so the fixture does not mirror src.
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            byte ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// Base64 of the raw-deflated rights XML whose encryptedKey wraps `book_key` to
/// `common::key()`.
fn adept_license(book_key: &[u8]) -> String {
    let wrapped = common::key()
        .to_public_key()
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, book_key)
        .unwrap();
    let xml = format!(
        "<rights xmlns=\"http://ns.adobe.com/adept\"><licenseToken><encryptedKey>{}\
         </encryptedKey></licenseToken></rights>",
        STANDARD.encode(wrapped)
    );
    let mut deflate =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    deflate.write_all(xml.as_bytes()).unwrap();
    STANDARD.encode(deflate.finish().unwrap())
}

/// Hand-assembled AES PDF whose catalog, pages, page and info live in object stream 5,
/// encrypted as a whole with object 5's key; xref stream 8 maps them as compressed.
/// `corrupt` gives the object stream a /First beyond its content so it cannot parse.
fn object_stream_pdf(corrupt: bool) -> Vec<u8> {
    let packed: [(u32, &[u8]); 4] = [
        (1, b"<</Type/Catalog/Pages 2 0 R>>"),
        (2, b"<</Type/Pages/Kids[3 0 R]/Count 1>>"),
        (3, b"<</Type/Page/Parent 2 0 R/Contents 4 0 R>>"),
        (7, b"<</Title(Packed Title)>>"),
    ];
    let mut header = String::new();
    let mut body = Vec::new();
    for (number, object) in packed {
        header.push_str(&format!("{number} {} ", body.len()));
        body.extend_from_slice(object);
        body.push(b' ');
    }
    let first = if corrupt { 9999 } else { header.len() };
    let mut stm_plain = header.into_bytes();
    stm_plain.extend_from_slice(&body);
    let aes = |number, plain: &[u8]| {
        encrypt_value(
            plain,
            &object_key(&KEY, (number, 0), Cipher::Aes),
            Cipher::Aes,
            9,
        )
    };

    let mut pdf = b"%PDF-1.5\n".to_vec();
    let mut offsets = [0usize; 9];
    let stream_object = |pdf: &mut Vec<u8>, number: u32, dict: &str, data: &[u8]| {
        pdf.extend_from_slice(
            format!("{number} 0 obj\n<<{dict}/Length {}>>\nstream\n", data.len()).as_bytes(),
        );
        pdf.extend_from_slice(data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    };
    offsets[4] = pdf.len();
    stream_object(&mut pdf, 4, "", &aes(4, CONTENT));
    offsets[5] = pdf.len();
    stream_object(
        &mut pdf,
        5,
        &format!("/Type/ObjStm/N 4/First {first}"),
        &aes(5, &stm_plain),
    );
    offsets[6] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "6 0 obj\n<</Filter/EBX_HANDLER/V 4/CF<</StdCF<</CFM/AESV2>>>>/StmF/StdCF/StrF/StdCF\
             /ADEPT_LICENSE({})>>\nendobj\n",
            adept_license(&KEY)
        )
        .as_bytes(),
    );
    offsets[8] = pdf.len();
    let mut xref = Vec::new();
    let mut entry = |kind: u8, field: usize, index: u16| {
        xref.push(kind);
        xref.extend_from_slice(&(field as u32).to_be_bytes());
        xref.extend_from_slice(&index.to_be_bytes());
    };
    entry(0, 0, 65535);
    for number in 1..9u32 {
        match packed.iter().position(|(n, _)| *n == number) {
            Some(index) => entry(2, 5, index as u16),
            None => entry(1, offsets[number as usize], 0),
        }
    }
    stream_object(
        &mut pdf,
        8,
        "/Type/XRef/Size 9/W[1 4 2]/Root 1 0 R/Info 7 0 R/Encrypt 6 0 R",
        &xref,
    );
    pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", offsets[8]).as_bytes());
    pdf
}

#[test]
fn ebx_detection() {
    assert!(is_ebx_encrypted(&pdf(Some(Cipher::Aes))));
    assert!(is_ebx_encrypted(&pdf(Some(Cipher::Rc4))));
    assert!(!is_ebx_encrypted(&pdf(None)));
    assert!(!is_ebx_encrypted(b"not a pdf"));
}

#[test]
fn plain_and_unparseable_pdfs_are_rejected() {
    assert!(matches!(
        decrypt_pdf(&pdf(None), &KEY),
        Err(PdfDrmError::NotEbx)
    ));
    assert!(matches!(
        decrypt_pdf(b"not a pdf", &KEY),
        Err(PdfDrmError::Parse(_))
    ));
    assert!(matches!(
        content_key(&pdf(None), common::key()),
        Err(PdfDrmError::NotEbx)
    ));
}

#[test]
fn content_key_unwraps_adept_license() {
    assert_eq!(
        content_key(&pdf(Some(Cipher::Aes)), common::key()).unwrap(),
        KEY
    );
    assert!(matches!(
        content_key(&pdf(Some(Cipher::Aes)), common::other_key()),
        Err(PdfDrmError::License(_))
    ));
}

#[test]
fn aes_pdf_decrypts_streams_and_strings_and_drops_encrypt() {
    assert_decrypts(&pdf(Some(Cipher::Aes)), &KEY);
}

#[test]
fn aes_pdf_with_xref_stream_decrypts() {
    let encrypted = pdf_with(Some(Cipher::Aes), &KEY, true);
    // No classic trailer: /Encrypt sits in the xref stream dictionary.
    assert!(!encrypted.windows(7).any(|w| w == b"trailer"));
    assert!(is_ebx_encrypted(&encrypted));
    assert_decrypts(&encrypted, &KEY);
}

#[test]
fn rc4_pdf_decrypts() {
    assert_decrypts(&pdf(Some(Cipher::Rc4)), &KEY);
    // A 5-byte book key gives 10-byte object keys.
    let short_key = *b"40bit";
    let encrypted = pdf_with(Some(Cipher::Rc4), &short_key, false);
    assert_eq!(content_key(&encrypted, common::key()).unwrap(), short_key);
    assert_decrypts(&encrypted, &short_key);
}

#[test]
fn string_with_bad_padding_is_an_error() {
    let (mut document, info_id) = plain_document();
    encrypt(&mut document, Cipher::Aes, &KEY);
    // IV + one block that decrypts to sixteen zero bytes: invalid PKCS#7 padding.
    let key = object_key(&KEY, info_id, Cipher::Aes);
    let bad = common::aes_encrypt(
        key.as_slice().try_into().unwrap(),
        &[3; 16],
        &[0; 16],
        false,
    );
    document
        .get_object_mut(info_id)
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set(
            "Subject",
            Object::String(bad.clone(), StringFormat::Hexadecimal),
        );
    match decrypt_pdf(&save(document, false), &KEY) {
        Err(PdfDrmError::Decrypt(message)) => {
            assert!(
                message.contains(&format!("object {} ", info_id.0)),
                "{message}"
            )
        }
        other => panic!("expected a decrypt error, got {other:?}"),
    }
}

#[test]
fn object_key_byte_order_is_pinned() {
    use md5::{Digest, Md5};
    // Object number 0x7f030201 keeps only its low three bytes, little-endian; generation
    // 0x0405 is two bytes little-endian.
    let aes_input = b"pdf-content-key!\x01\x02\x03\x05\x04sAlT";
    assert_eq!(
        object_key(&KEY, (0x7f03_0201, 0x0405), Cipher::Aes),
        Md5::digest(aes_input)[..16]
    );
    let rc4_input = b"40bit\x01\x02\x03\x05\x04";
    assert_eq!(
        object_key(b"40bit", (0x7f03_0201, 0x0405), Cipher::Rc4),
        Md5::digest(rc4_input)[..10]
    );
}

#[test]
fn high_object_numbers_and_generations_decrypt() {
    for cipher in [Cipher::Aes, Cipher::Rc4] {
        // 300 needs the second key byte, 0x010203 the third; both carry a generation.
        let (mut document, _) = plain_document_at((300, 2), (0x01_0203, 1));
        encrypt(&mut document, cipher, &KEY);
        assert_decrypts(&save(document, false), &KEY);
    }
}

#[test]
fn v4_with_v2_crypt_filter_uses_rc4() {
    let scheme = Scheme {
        entries: v4_entries(
            dictionary! { "StdCF" => dictionary! { "CFM" => "V2" } },
            "StdCF",
            "StdCF",
        ),
        ..scheme(Cipher::Rc4)
    };
    assert_decrypts(&encrypted_with(&scheme), &KEY);
}

#[test]
fn stream_and_string_filters_are_selected_separately() {
    let filters = dictionary! {
        "AesCF" => dictionary! { "CFM" => "AESV2" },
        "Rc4CF" => dictionary! { "CFM" => "V2" },
    };
    let mixed = Scheme {
        entries: v4_entries(filters.clone(), "AesCF", "Rc4CF"),
        stream: Cipher::Aes,
        string: Cipher::Rc4,
        encrypt_metadata: true,
    };
    assert_decrypts(&encrypted_with(&mixed), &KEY);
    let identity_strings = Scheme {
        entries: v4_entries(filters, "AesCF", "Identity"),
        string: Cipher::Identity,
        ..mixed
    };
    assert_decrypts(&encrypted_with(&identity_strings), &KEY);
}

#[test]
fn unencrypted_metadata_stays_plain() {
    let (mut document, _) = plain_document();
    let metadata_id = document.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        XMP.to_vec(),
    ));
    let mut scheme = scheme(Cipher::Aes);
    scheme.entries.set("EncryptMetadata", false);
    scheme.encrypt_metadata = false;
    encrypt_with(&mut document, &scheme, &KEY);
    let plain = decrypt_pdf(&save(document, false), &KEY).unwrap();
    let document = Document::load_mem(&plain).unwrap();
    let metadata = document
        .get_object(metadata_id)
        .unwrap()
        .as_stream()
        .unwrap();
    assert_eq!(metadata.content, XMP);
    let page_id = *document.get_pages().get(&1).unwrap();
    assert_eq!(
        document.get_page_content(page_id).unwrap().trim_ascii_end(),
        CONTENT
    );
}

#[test]
fn v5_is_unsupported() {
    let v5 = Scheme {
        entries: dictionary! { "V" => 5 },
        ..scheme(Cipher::Aes)
    };
    assert!(matches!(
        decrypt_pdf(&encrypted_with(&v5), &KEY),
        Err(PdfDrmError::Unsupported(_))
    ));
    let aesv3 = Scheme {
        entries: v4_entries(
            dictionary! { "StdCF" => dictionary! { "CFM" => "AESV3" } },
            "StdCF",
            "StdCF",
        ),
        ..scheme(Cipher::Aes)
    };
    assert!(matches!(
        decrypt_pdf(&encrypted_with(&aesv3), &KEY),
        Err(PdfDrmError::Unsupported(_))
    ));
}

#[test]
fn wrong_rc4_key_is_an_error() {
    match decrypt_pdf(&pdf(Some(Cipher::Rc4)), b"wrong-content-k!") {
        Err(PdfDrmError::Decrypt(message)) => assert!(message.contains("wrong key"), "{message}"),
        other => panic!("expected a decrypt error, got {other:?}"),
    }
}

fn encrypted_with(scheme: &Scheme) -> Vec<u8> {
    let (mut document, _) = plain_document();
    encrypt_with(&mut document, scheme, &KEY);
    save(document, false)
}

#[test]
fn wrong_key_length_is_rejected() {
    assert!(matches!(
        decrypt_pdf(&pdf(Some(Cipher::Aes)), b"40bit"),
        Err(PdfDrmError::KeyLength(5))
    ));
    let rc4 = pdf(Some(Cipher::Rc4));
    assert!(matches!(
        decrypt_pdf(&rc4, b"four"),
        Err(PdfDrmError::KeyLength(4))
    ));
    assert!(matches!(
        decrypt_pdf(&rc4, &[0; 17]),
        Err(PdfDrmError::KeyLength(17))
    ));
}

#[test]
fn object_stream_objects_are_recovered() {
    let encrypted = object_stream_pdf(false);
    assert!(is_ebx_encrypted(&encrypted));
    assert_eq!(content_key(&encrypted, common::key()).unwrap(), KEY);
    let plain = decrypt_pdf(&encrypted, &KEY).unwrap();
    let document = Document::load_mem(&plain).unwrap();
    assert!(document.trailer.get(b"Encrypt").is_err());
    let page_id = *document.get_pages().get(&1).unwrap();
    assert_eq!(
        document.get_page_content(page_id).unwrap().trim_ascii_end(),
        CONTENT
    );
    // Strings inside an object stream are covered by the container's encryption only.
    assert_eq!(
        info(&document).get(b"Title").unwrap().as_str().unwrap(),
        b"Packed Title"
    );
}

#[test]
fn corrupt_object_stream_is_an_error() {
    assert!(matches!(
        decrypt_pdf(&object_stream_pdf(true), &KEY),
        Err(PdfDrmError::Decrypt(_))
    ));
}

fn info(document: &Document) -> &lopdf::Dictionary {
    document
        .trailer
        .get(b"Info")
        .and_then(Object::as_reference)
        .and_then(|id| document.get_dictionary(id))
        .unwrap()
}

fn assert_decrypts(encrypted: &[u8], book_key: &[u8]) {
    let plain = decrypt_pdf(encrypted, book_key).unwrap();
    let document = Document::load_mem(&plain).unwrap();
    assert!(document.trailer.get(b"Encrypt").is_err());
    let page_id = *document.get_pages().get(&1).unwrap();
    // get_page_content re-encodes the operations, which appends a newline.
    assert_eq!(
        document.get_page_content(page_id).unwrap().trim_ascii_end(),
        CONTENT
    );
    assert_eq!(
        info(&document).get(b"Title").unwrap().as_str().unwrap(),
        TITLE
    );
    let content_id = document.get_page_contents(page_id)[0];
    let content = document
        .get_object(content_id)
        .unwrap()
        .as_stream()
        .unwrap();
    assert_eq!(content.dict.get(b"Note").unwrap().as_str().unwrap(), NOTE);
}
