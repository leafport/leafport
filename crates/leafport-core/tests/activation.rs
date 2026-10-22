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
    use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
    let (iv, ciphertext) = wrapped.split_at(16);
    cbc::Decryptor::<aes::Aes128>::new_from_slices(&DEVICE_KEY, iv)
        .unwrap()
        .decrypt_padded_vec::<Pkcs7>(ciphertext)
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

mod device_activation {
    use super::common::{self, FakeTransport};
    use leafport_core::activation::{
        ACTIVATION_SERVICE_INFO, ActivationTokenError, DeviceActivationError, activate_request,
        ensure_device_activation, parse_activation_token,
    };

    const SERVICE_INFO: &str = r#"<activationServiceInfo xmlns="http://ns.adobe.com/adept"><authURL>http://a.example/adept</authURL><userInfoURL>http://a.example/adept</userInfoURL><activationURL>https://act.example/adept</activationURL><certificate>AA==</certificate></activationServiceInfo>"#;

    fn token(user: &str, fingerprint: &str) -> String {
        format!(
            r#"<activationToken xmlns="http://ns.adobe.com/adept"><device>urn:uuid:server-device</device><fingerprint>{fingerprint}</fingerprint><deviceType>standalone</deviceType><activationURL>http://act.example/adept</activationURL><user>{user}</user><signature>c2ln</signature></activationToken>"#
        )
    }

    fn unactivated() -> leafport_core::store::ActivationState {
        let mut state = common::activation();
        state.device = "activation://device/local".into();
        state.activation_token_xml = None;
        state
    }

    #[test]
    fn activate_request_follows_native_field_order_and_verifies() {
        let signed = activate_request("urn:uuid:u", "FP=", common::key()).unwrap();
        common::verify_signed(&signed, common::key());
        let document = roxmltree::Document::parse(&signed).unwrap();
        let root = document.root_element();
        assert_eq!(root.tag_name().name(), "activate");
        assert_eq!(root.attribute("requestType"), Some("initial"));
        let names: Vec<&str> = root
            .children()
            .filter(|node| node.is_element())
            .map(|node| node.tag_name().name())
            .collect();
        assert_eq!(
            names,
            [
                "fingerprint",
                "deviceType",
                "clientOS",
                "clientLocale",
                "clientVersion",
                "targetDevice",
                "nonce",
                "expiration",
                "user",
                "signature"
            ]
        );
        let target = root
            .children()
            .find(|node| node.has_tag_name("targetDevice"))
            .unwrap();
        let token = target
            .children()
            .find(|node| node.has_tag_name("activationToken"))
            .unwrap();
        // Only the user: the device has no server identity before activation.
        assert_eq!(token.children().filter(|node| node.is_element()).count(), 1);
        assert!(signed.contains("<deviceType>standalone</deviceType>"));
        assert!(signed.contains("<productName>Leafport</productName>"));
    }

    #[test]
    fn token_parses_and_keeps_root_element_verbatim() {
        let xml = format!("<?xml version=\"1.0\"?>{}", token("urn:uuid:u", "FP="));
        let parsed = parse_activation_token(&xml).unwrap();
        assert_eq!(parsed.device, "urn:uuid:server-device");
        assert_eq!(parsed.user, "urn:uuid:u");
        assert_eq!(parsed.fingerprint, "FP=");
        assert_eq!(parsed.device_type, "standalone");
        assert_eq!(parsed.activation_url, "http://act.example/adept");
        assert_eq!(parsed.xml, token("urn:uuid:u", "FP="));
        assert_eq!(parse_activation_token(&parsed.xml).unwrap(), parsed);
    }

    #[test]
    fn nested_token_keeps_the_whole_document_so_it_reparses() {
        let xml = format!(
            r#"<a:wrapper xmlns:a="http://ns.adobe.com/adept">{}</a:wrapper>"#,
            token("urn:uuid:u", "FP=")
                .replace(" xmlns=\"http://ns.adobe.com/adept\"", "")
                .replace('<', "<a:")
                .replace("<a:/", "</a:")
        );
        let parsed = parse_activation_token(&xml).unwrap();
        assert_eq!(parse_activation_token(&parsed.xml).unwrap(), parsed);
    }

    #[test]
    fn error_root_is_a_service_error() {
        let error = parse_activation_token(
            r#"<error xmlns="http://ns.adobe.com/adept" data="E_ACT_TOO_MANY_ACTIVATIONS"/>"#,
        )
        .unwrap_err();
        assert!(matches!(error, ActivationTokenError::Service { .. }));
        assert!(error.to_string().contains("E_ACT_TOO_MANY_ACTIVATIONS"));
    }

    #[test]
    fn ensure_activates_once_and_adopts_the_server_device() {
        let mut state = unactivated();
        let transport = FakeTransport::new()
            .respond("/ActivationServiceInfo", 200, SERVICE_INFO)
            .respond(
                "https://act.example/adept/Activate",
                200,
                token(&state.user, &state.fingerprint),
            );
        assert!(ensure_device_activation(&transport, &mut state).unwrap());
        assert_eq!(state.device, "urn:uuid:server-device");
        assert_eq!(
            state.activation_token().unwrap().device,
            "urn:uuid:server-device"
        );
        // Operators initialized under no (or another) registration do not carry over.
        assert!(state.operator_urls.is_empty());
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].0, ACTIVATION_SERVICE_INFO);
        common::verify_signed(&requests[1].1, common::key());
        drop(requests);

        // Activated: no further network traffic.
        assert!(!ensure_device_activation(&transport, &mut state).unwrap());
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn service_info_without_activation_url_activates_at_auth_url() {
        // Shape of the live adeactivate.adobe.com response: no activationURL element.
        const LIVE_INFO: &str = r#"<activationServiceInfo xmlns="http://ns.adobe.com/adept"><authURL>http://a.example/adept</authURL><userInfoURL>http://a.example/adept</userInfoURL><certificate>AA==</certificate></activationServiceInfo>"#;
        let mut state = unactivated();
        let transport = FakeTransport::new()
            .respond("/ActivationServiceInfo", 200, LIVE_INFO)
            .respond(
                "http://a.example/adept/Activate",
                200,
                token(&state.user, &state.fingerprint),
            );
        assert!(ensure_device_activation(&transport, &mut state).unwrap());
        assert_eq!(state.device, "urn:uuid:server-device");
        assert_eq!(
            transport.requests.lock().unwrap()[1].0,
            "http://a.example/adept/Activate"
        );
    }

    #[test]
    fn stored_token_for_another_user_fails_without_network() {
        let mut state = unactivated();
        state.activation_token_xml = Some(token("urn:uuid:someone-else", &state.fingerprint));
        let transport = FakeTransport::new();
        let error = ensure_device_activation(&transport, &mut state).unwrap_err();
        assert!(matches!(error, DeviceActivationError::Mismatch("user")));
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn stored_token_with_another_fingerprint_fails_without_network() {
        let mut state = unactivated();
        state.activation_token_xml = Some(token(&state.user, "OTHER="));
        let transport = FakeTransport::new();
        let error = ensure_device_activation(&transport, &mut state).unwrap_err();
        assert!(matches!(
            error,
            DeviceActivationError::Mismatch("fingerprint")
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn corrupt_stored_token_fails_without_network() {
        let mut state = unactivated();
        state.activation_token_xml = Some("<activationToken".into());
        let transport = FakeTransport::new();
        let error = ensure_device_activation(&transport, &mut state).unwrap_err();
        assert!(matches!(error, DeviceActivationError::StoredToken(_)));
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_device_is_reconciled_from_stored_token_without_network() {
        let mut state = unactivated();
        state.activation_token_xml = Some(token(&state.user, &state.fingerprint));
        let operators = state.operator_urls.clone();
        let transport = FakeTransport::new();
        assert!(ensure_device_activation(&transport, &mut state).unwrap());
        assert_eq!(state.device, "urn:uuid:server-device");
        assert_eq!(state.operator_urls, operators);
        assert!(!ensure_device_activation(&transport, &mut state).unwrap());
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn mismatched_fingerprint_is_rejected_and_state_untouched() {
        let mut state = unactivated();
        let transport = FakeTransport::new()
            .respond("/ActivationServiceInfo", 200, SERVICE_INFO)
            .respond("/Activate", 200, token(&state.user, "OTHER="));
        let error = ensure_device_activation(&transport, &mut state).unwrap_err();
        assert!(matches!(
            error,
            DeviceActivationError::Mismatch("fingerprint")
        ));
        assert_eq!(state.device, "activation://device/local");
        assert!(state.activation_token_xml.is_none());
    }

    #[test]
    fn service_error_on_http_failure_is_surfaced() {
        let mut state = unactivated();
        let transport = FakeTransport::new()
            .respond("/ActivationServiceInfo", 200, SERVICE_INFO)
            .respond(
                "/Activate",
                403,
                r#"<error xmlns="http://ns.adobe.com/adept" data="E_ACT_TOO_MANY_ACTIVATIONS"/>"#,
            );
        let error = ensure_device_activation(&transport, &mut state).unwrap_err();
        assert!(error.to_string().contains("E_ACT_TOO_MANY_ACTIVATIONS"));
        assert!(state.activation_token_xml.is_none());
    }
}
