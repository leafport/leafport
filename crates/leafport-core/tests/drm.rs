mod common;

use leafport_core::drm::{DrmError, content_key, decrypt_resource, encrypted_resources};
use leafport_core::rsa::Pkcs1v15Encrypt;
use base64::{Engine, engine::general_purpose::STANDARD};

const KEY: [u8; 16] = *b"0123456789abcdef";
const IV: [u8; 16] = [7; 16];

fn rights(wrapped: &[u8]) -> String {
    let encoded = STANDARD.encode(wrapped);
    let (head, tail) = encoded.split_at(encoded.len() / 2);
    format!(
        r#"<rights xmlns="http://ns.adobe.com/adept"><licenseToken><encryptedKey keyInfo="x">
            {head}
            {tail}
        </encryptedKey></licenseToken></rights>"#
    )
}

#[test]
fn content_key_unwraps_with_the_license_key() {
    let wrapped = common::key()
        .to_public_key()
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &KEY)
        .unwrap();
    assert_eq!(content_key(&rights(&wrapped), common::key()).unwrap(), KEY);
}

#[test]
fn content_key_rejects_wrong_key_length_missing_key_and_bad_base64() {
    let short = common::key()
        .to_public_key()
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &KEY[..8])
        .unwrap();
    assert!(matches!(
        content_key(&rights(&short), common::key()),
        Err(DrmError::KeyLength(8))
    ));
    assert!(matches!(
        content_key(
            r#"<rights xmlns="http://ns.adobe.com/adept"/>"#,
            common::key()
        ),
        Err(DrmError::MissingKey)
    ));
    assert!(matches!(
        content_key(
            r#"<encryptedKey xmlns="http://ns.adobe.com/adept">!!!</encryptedKey>"#,
            common::key()
        ),
        Err(DrmError::KeyEncoding(_))
    ));
}

#[test]
fn content_key_from_another_device_fails_to_unwrap() {
    let wrapped = common::other_key()
        .to_public_key()
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &KEY)
        .unwrap();
    assert!(matches!(
        content_key(&rights(&wrapped), common::key()),
        Err(DrmError::Unwrap(_))
    ));
}

#[test]
fn encryption_manifest_lists_resources_and_compression() {
    let xml = r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#" xmlns:a="http://ns.adobe.com/adept">
        <enc:EncryptedData>
            <enc:CipherData><enc:CipherReference URI="OEBPS/ch%201.xhtml"/></enc:CipherData>
            <enc:EncryptionProperties><enc:EncryptionProperty>
                <a:Compression Method="8" OriginalLength="10"/>
            </enc:EncryptionProperty></enc:EncryptionProperties>
        </enc:EncryptedData>
        <enc:EncryptedData>
            <enc:CipherData><enc:CipherReference URI="OEBPS/img.png"/></enc:CipherData>
        </enc:EncryptedData>
        <enc:EncryptedData><enc:CipherData/></enc:EncryptedData>
    </encryption>"#;
    let resources = encrypted_resources(xml).unwrap();
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].uri, "OEBPS/ch%201.xhtml");
    assert!(resources[0].compressed);
    assert_eq!(resources[1].uri, "OEBPS/img.png");
    assert!(!resources[1].compressed);
}

#[test]
fn uncompressed_resource_decrypts_without_stripping_padding() {
    let plain = b"exactly sixteen!exactly sixteen!";
    let data = common::aes_encrypt(&KEY, &IV, plain, false);
    assert_eq!(decrypt_resource(&data, &KEY, false).unwrap(), plain);
}

#[test]
fn compressed_resource_is_inflated_after_decryption() {
    use std::io::Write;
    let plain = b"<html><body>Hello, reader. Hello, reader.</body></html>";
    let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    deflate.write_all(plain).unwrap();
    let compressed = deflate.finish().unwrap();
    let data = common::aes_encrypt(&KEY, &IV, &compressed, true);
    assert_eq!(decrypt_resource(&data, &KEY, true).unwrap(), plain);
}

#[test]
fn truncated_and_misaligned_resources_are_rejected() {
    assert!(matches!(
        decrypt_resource(&[0; 31], &KEY, false),
        Err(DrmError::Truncated)
    ));
    assert!(matches!(
        decrypt_resource(&[0; 33], &KEY, false),
        Err(DrmError::Misaligned)
    ));
}
