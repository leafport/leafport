//! Shared fixtures for leafport-core integration tests.

#![allow(dead_code)]

use std::sync::OnceLock;

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
