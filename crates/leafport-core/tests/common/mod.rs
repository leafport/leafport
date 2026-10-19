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
