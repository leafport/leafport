mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use leafport_core::activation::{
    ActivationKeys, SignInDataError, SignInResponseError, device_fingerprint, device_id,
    encrypt_sign_in_data, export_private_key, parse_sign_in_response, sign_in_request, sign_in_url,
    wrap_private_key,
};
use leafport_core::rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

const DEVICE_KEY: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];

fn unwrap_key(wrapped: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    let (iv, ciphertext) = wrapped.split_at(16);
    cbc::Decryptor::<aes::Aes128>::new((&DEVICE_KEY).into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .unwrap()
}

#[test]
fn device_identity_is_derived_from_the_key() {
    assert_eq!(
        device_id(&DEVICE_KEY),
        "activation://device/00112233-4455-6677-8899-aabbccddeeff"
    );
    use sha1::{Digest, Sha1};
    assert_eq!(
        STANDARD.decode(device_fingerprint(&DEVICE_KEY)).unwrap(),
        Sha1::digest(DEVICE_KEY).to_vec()
    );
}

#[test]
fn sign_in_url_joins_operation() {
    assert_eq!(
        sign_in_url("https://h/adept/"),
        "https://h/adept/SignInDirect"
    );
}

#[test]
fn sign_in_data_packs_device_key_and_length_prefixed_credentials() {
    let encrypted = encrypt_sign_in_data(
        &DEVICE_KEY,
        "me@x",
        "pässword",
        &common::key().to_public_key(),
    )
    .unwrap();
    let plain = common::key().decrypt(Pkcs1v15Encrypt, &encrypted).unwrap();
    let mut expected = DEVICE_KEY.to_vec();
    expected.push(4);
    expected.extend_from_slice(b"me@x");
    expected.push("pässword".len() as u8);
    expected.extend_from_slice("pässword".as_bytes());
    assert_eq!(plain, expected);
}

#[test]
fn sign_in_data_rejects_credentials_over_255_bytes() {
    let long = "x".repeat(256);
    let public = common::key().to_public_key();
    assert!(matches!(
        encrypt_sign_in_data(&DEVICE_KEY, &long, "p", &public),
        Err(SignInDataError::CredentialTooLong)
    ));
    assert!(matches!(
        encrypt_sign_in_data(&DEVICE_KEY, "u", &long, &public),
        Err(SignInDataError::CredentialTooLong)
    ));
}

#[test]
fn wrapped_private_key_decrypts_with_device_key_and_uses_fresh_iv() {
    let secret = b"pkcs8 bytes of any length";
    let first = wrap_private_key(&DEVICE_KEY, secret);
    let second = wrap_private_key(&DEVICE_KEY, secret);
    assert_ne!(first[..16], second[..16]);
    assert_eq!(unwrap_key(&first), secret);
    assert_eq!(unwrap_key(&second), secret);
}

#[test]
fn sign_in_request_carries_keys_in_native_order() {
    let keys = ActivationKeys {
        authentication: common::key().clone(),
        license: common::other_key().clone(),
    };
    let xml = sign_in_request("Vendor\"&<", b"DATA", &DEVICE_KEY, &keys).unwrap();
    let document = roxmltree::Document::parse(&xml).unwrap();
    let root = document.root_element();
    assert_eq!(root.attribute("method"), Some("Vendor\"&<"));
    let children: Vec<_> = root
        .children()
        .map(|node| (node.tag_name().name(), node.text().unwrap_or("")))
        .collect();
    let names: Vec<_> = children.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "signInData",
            "publicAuthKey",
            "encryptedPrivateAuthKey",
            "publicLicenseKey",
            "encryptedPrivateLicenseKey"
        ]
    );
    assert_eq!(children[0].1, STANDARD.encode(b"DATA"));
    assert!(
        root.children()
            .all(|node| node.tag_name().name() != "signature")
    );

    use leafport_core::rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
    let public = leafport_core::rsa::RsaPublicKey::from_public_key_der(
        &STANDARD.decode(children[1].1).unwrap(),
    )
    .unwrap();
    assert_eq!(public, keys.authentication.to_public_key());
    let private = unwrap_key(&STANDARD.decode(children[4].1).unwrap());
    assert_eq!(private, export_private_key(&keys.license).unwrap());
    assert_eq!(
        RsaPrivateKey::from_pkcs8_der(&private).unwrap(),
        keys.license
    );
}

#[test]
fn sign_in_response_returns_user_and_retains_credentials() {
    let xml = r#"<credentials xmlns="http://ns.adobe.com/adept"><user>urn:uuid:me</user><pkcs12>P</pkcs12></credentials>"#;
    let result = parse_sign_in_response(xml).unwrap();
    assert_eq!(result.user, "urn:uuid:me");
    assert_eq!(result.credentials_xml, xml);
}

#[test]
fn sign_in_response_errors() {
    assert!(matches!(
        parse_sign_in_response(r#"<error xmlns="http://ns.adobe.com/adept" data="E_AUTH"/>"#),
        Err(SignInResponseError::Service { .. })
    ));
    assert!(matches!(
        parse_sign_in_response(
            r#"<credentials xmlns="http://ns.adobe.com/adept"><user/></credentials>"#
        ),
        Err(SignInResponseError::MissingUser)
    ));
    assert!(matches!(
        parse_sign_in_response("<credentials><user>u</user></credentials>"),
        Err(SignInResponseError::MissingUser)
    ));
    assert!(matches!(
        parse_sign_in_response("<"),
        Err(SignInResponseError::Xml(_))
    ));
}

#[test]
fn account_auth_key_reads_pkcs12_with_base64_device_key() {
    use leafport_core::activation::account_auth_key;
    use leafport_core::rsa::pkcs8::EncodePrivateKey;
    let key = common::key();
    let der = key.to_pkcs8_der().unwrap();
    let pfx = p12::PFX::new(
        b"cert",
        der.as_bytes(),
        None,
        &STANDARD.encode(DEVICE_KEY),
        "auth",
    )
    .unwrap()
    .to_der();
    let xml = format!(
        r#"<credentials xmlns="http://ns.adobe.com/adept"><user>urn:uuid:me</user><pkcs12>{}</pkcs12></credentials>"#,
        STANDARD.encode(pfx)
    );
    assert_eq!(account_auth_key(&xml, &DEVICE_KEY).as_ref(), Some(key));
    let mut other = DEVICE_KEY;
    other[0] ^= 1;
    assert!(account_auth_key(&xml, &other).is_none());
    assert!(
        account_auth_key(
            "<credentials xmlns=\"http://ns.adobe.com/adept\"/>",
            &DEVICE_KEY
        )
        .is_none()
    );
}
