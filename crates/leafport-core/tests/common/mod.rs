//! Shared fixtures for leafport-core integration tests.

#![allow(dead_code)]

use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use leafport_core::service::{AdeptResponse, ServiceError, Transport};

use leafport_core::rsa::RsaPrivateKey;

/// One 1024-bit key per test binary; generation is slow in debug builds.
pub fn key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap())
}

/// A second, distinct key for tests that need two identities.
pub fn other_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap())
}

/// Verify the root signature appended by `sign_request` against `key`.
pub fn verify_signed(xml: &str, key: &RsaPrivateKey) {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let document = roxmltree::Document::parse(xml).unwrap();
    let root = document.root_element();
    let signature = root
        .children()
        .find(|node| node.has_tag_name(("http://ns.adobe.com/adept", "signature")))
        .expect("root signature");
    let bytes = STANDARD.decode(signature.text().unwrap()).unwrap();
    leafport_core::signing::verify(root, &key.to_public_key(), &bytes).unwrap();
}

/// AES-128-CBC encrypt with a prepended IV, as ADEPT content uses.
pub fn aes_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8], pad: bool) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding, block_padding::Pkcs7};
    let encryptor = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into());
    let ciphertext = if pad {
        encryptor.encrypt_padded_vec_mut::<Pkcs7>(plain)
    } else {
        encryptor.encrypt_padded_vec_mut::<NoPadding>(plain)
    };
    let mut out = iv.to_vec();
    out.extend_from_slice(&ciphertext);
    out
}

/// Scripted transport: responses are consumed in order by `post_adept` and `fetch`.
pub struct FakeTransport {
    queue: Mutex<VecDeque<(String, u16, Vec<u8>)>>,
    /// Recorded `(url, body)` pairs; `fetch` records an empty body.
    pub requests: Mutex<Vec<(String, String)>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn respond(self, url_suffix: &str, status: u16, body: impl Into<Vec<u8>>) -> Self {
        self.queue
            .lock()
            .unwrap()
            .push_back((url_suffix.to_owned(), status, body.into()));
        self
    }

    fn next(&self, url: &str, body: String) -> AdeptResponse {
        self.requests.lock().unwrap().push((url.to_owned(), body));
        let (suffix, status, body) = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected request to {url}: queue empty"));
        assert!(
            url.ends_with(&suffix),
            "request {url} does not end with {suffix}"
        );
        AdeptResponse {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            body,
        }
    }
}

impl Transport for FakeTransport {
    fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
        Ok(self.next(url, xml))
    }

    fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
        let response = self.next(url, String::new());
        if response.body.len() > max_bytes {
            return Err(ServiceError::ResponseTooLarge);
        }
        Ok(response)
    }
}

/// Content key of the protected EPUB fixtures, wrapped to `key()` in their rights.xml.
pub const KEY: [u8; 16] = *b"0123456789abcdef";
pub const IV: [u8; 16] = [7; 16];
pub const CHAPTER: &[u8] = b"<html><body><p>Chapter one</p></body></html>";
pub const ADEPT_DATA: &str = r#"<enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
        <enc:CipherData><enc:CipherReference URI="OEBPS/ch%31.xhtml"/></enc:CipherData>
    </enc:EncryptedData>"#;
pub const CONTAINER: &[u8] = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// Zip builder: entries in order, `mimetype` stored, everything else deflated.
pub fn epub(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        let method = if *name == "mimetype" {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        zip.start_file(
            *name,
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn encryption_xml(data: &[&str]) -> String {
    format!(
        r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
    xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
    {}
</encryption>"#,
        data.join("\n    ")
    )
}

fn rights() -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use leafport_core::rsa::Pkcs1v15Encrypt;
    let wrapped = key()
        .to_public_key()
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &KEY)
        .unwrap();
    format!(
        r#"<rights xmlns="http://ns.adobe.com/adept"><licenseToken><encryptedKey keyInfo="x">{}</encryptedKey></licenseToken></rights>"#,
        STANDARD.encode(wrapped)
    )
}

/// A protected EPUB whose `OEBPS/ch1.xhtml` entry holds `chapter` as stored ciphertext.
pub fn protected_epub_with(chapter: &[u8], extra: &[(&str, &[u8])], data: &[&str]) -> Vec<u8> {
    let encryption = encryption_xml(data);
    let rights = rights();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER),
        ("META-INF/rights.xml", rights.as_bytes()),
        ("META-INF/encryption.xml", encryption.as_bytes()),
        ("OEBPS/ch1.xhtml", chapter),
    ];
    entries.extend_from_slice(extra);
    epub(&entries)
}

pub fn protected_epub(key: [u8; 16], extra: &[(&str, &[u8])], data: &[&str]) -> Vec<u8> {
    protected_epub_with(&aes_encrypt(&key, &IV, CHAPTER, true), extra, data)
}

/// One chapter encrypted with `key` (wrapped to `key()` in rights.xml).
pub fn adept_epub(key: [u8; 16]) -> Vec<u8> {
    protected_epub(key, &[], &[ADEPT_DATA])
}

/// Activation state whose authentication and license keys are both `key()`.
pub fn activation() -> leafport_core::store::ActivationState {
    leafport_core::store::ActivationState {
        device_key: [0; 16],
        keys: leafport_core::activation::ActivationKeys {
            authentication: key().clone(),
            license: key().clone(),
        },
        user: "urn:uuid:user".into(),
        device: "urn:uuid:device".into(),
        fingerprint: "fingerprint".into(),
        credentials_xml: String::new(),
    }
}

/// An ACSM for `resource` with operatorURL `https://op.example/fulfillment`.
pub fn acsm_xml(resource: &str, title: &str) -> String {
    format!(
        r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><distributor>urn:uuid:distributor</distributor><operatorURL>https://op.example/fulfillment</operatorURL><transaction>T-1</transaction><resourceItemInfo><resource>{resource}</resource><metadata><dc:title>{title}</dc:title><dc:format>application/epub+zip</dc:format></metadata></resourceItemInfo></fulfillmentToken>"#
    )
}

/// A fulfillment response for `resource`; `loan_until` adds a loanToken and permissions `until`.
pub fn fulfillment_xml(resource: &str, title: &str, src: &str, loan_until: Option<&str>) -> String {
    let permissions = loan_until
        .map(|u| format!("<permissions><display><until>{u}</until></display></permissions>"))
        .unwrap_or_default();
    let loan = loan_until
        .map(|_| "<loanToken><loan>L-1</loan></loanToken>".to_owned())
        .unwrap_or_default();
    format!(
        r#"<envelope xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><fulfillmentResult><fulfillment>F-1</fulfillment><resourceItemInfo><resource>{resource}</resource><metadata><dc:title>{title}</dc:title></metadata><src>{src}</src><downloadType>application/epub+zip</downloadType><licenseToken><encryptedKey>K</encryptedKey>{permissions}</licenseToken></resourceItemInfo>{loan}</fulfillmentResult></envelope>"#
    )
}
