mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use leafport_core::drm::{
    DrmError, MAX_RESOURCE_BYTES, content_key, decrypt_resource, encrypted_resources,
    is_adept_encryption,
};
use leafport_core::rsa::Pkcs1v15Encrypt;

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
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, &KEY)
        .unwrap();
    assert_eq!(content_key(&rights(&wrapped), common::key()).unwrap(), KEY);
}

#[test]
fn content_key_rejects_wrong_key_length_missing_key_and_bad_base64() {
    let short = common::key()
        .to_public_key()
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, &KEY[..8])
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
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, &KEY)
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
fn encryption_manifest_skips_non_adept_algorithms() {
    let xml = r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
        <enc:EncryptedData>
            <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
            <enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData>
        </enc:EncryptedData>
        <enc:EncryptedData>
            <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
            <enc:CipherData><enc:CipherReference URI="OEBPS/font.otf"/></enc:CipherData>
        </enc:EncryptedData>
    </encryption>"#;
    let resources = encrypted_resources(xml).unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "OEBPS/ch1.xhtml");
    assert!(is_adept_encryption(xml));
}

#[test]
fn adept_detection_ignores_font_obfuscation_only_manifests() {
    let font_only = r#"<encryption xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
        <enc:EncryptedData>
            <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
            <enc:CipherData><enc:CipherReference URI="OEBPS/font.otf"/></enc:CipherData>
        </enc:EncryptedData>
    </encryption>"#;
    assert!(!is_adept_encryption(font_only));
    assert!(encrypted_resources(font_only).unwrap().is_empty());
    let default_method = r#"<encryption xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
        <enc:EncryptedData>
            <enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData>
        </enc:EncryptedData>
    </encryption>"#;
    assert!(is_adept_encryption(default_method));
    assert!(!is_adept_encryption("<encryption/>"));
    assert!(!is_adept_encryption("not xml"));
}

#[test]
fn uncompressed_aligned_resource_without_padding_is_unchanged() {
    let plain = b"exactly sixteen!exactly sixteen!";
    let data = common::aes_encrypt(&KEY, &IV, plain, false);
    assert_eq!(decrypt_resource(&data, &KEY, false).unwrap(), plain);
}

#[test]
fn uncompressed_resource_has_pkcs7_padding_stripped() {
    let plain = b"<html><body/></html>";
    let data = common::aes_encrypt(&KEY, &IV, plain, true);
    assert_eq!(decrypt_resource(&data, &KEY, false).unwrap(), plain);
    let aligned = b"exactly sixteen!";
    let data = common::aes_encrypt(&KEY, &IV, aligned, true);
    assert_eq!(decrypt_resource(&data, &KEY, false).unwrap(), aligned);
}

#[test]
fn compressed_resource_over_the_cap_is_rejected() {
    use std::io::Write;
    let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..(MAX_RESOURCE_BYTES / chunk.len() + 1) {
        deflate.write_all(&chunk).unwrap();
    }
    let compressed = deflate.finish().unwrap();
    let data = common::aes_encrypt(&KEY, &IV, &compressed, true);
    assert!(matches!(
        decrypt_resource(&data, &KEY, true),
        Err(DrmError::TooLarge)
    ));
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
