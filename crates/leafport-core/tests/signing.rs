mod common;

use leafport_core::signing::{RequestSigningError, sign_request};

#[test]
fn signed_request_verifies_and_keeps_original_bytes() {
    let xml = r#"<?xml version="1.0"?><a:req xmlns:a="http://ns.adobe.com/adept"><a:user>u &amp; v</a:user></a:req>"#;
    let signed = sign_request(xml, common::key()).unwrap();
    let insertion = xml.rfind("</a:req>").unwrap();
    assert!(signed.starts_with(&xml[..insertion]));
    assert!(signed.ends_with("</signature></a:req>"));
    common::verify_signed(&signed, common::key());
}

#[test]
fn self_closing_root_is_expanded_before_signing() {
    let xml = r#"<req xmlns="http://ns.adobe.com/adept" k="v"/>"#;
    let signed = sign_request(xml, common::key()).unwrap();
    assert!(signed.starts_with(r#"<req xmlns="http://ns.adobe.com/adept" k="v"><signature"#));
    assert!(signed.ends_with("</signature></req>"));
    common::verify_signed(&signed, common::key());
}

#[test]
fn signature_fails_with_a_different_key_or_altered_content() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let signed = sign_request(
        r#"<req xmlns="http://ns.adobe.com/adept"><user>u</user></req>"#,
        common::key(),
    )
    .unwrap();
    let document = roxmltree::Document::parse(&signed).unwrap();
    let root = document.root_element();
    let signature = STANDARD
        .decode(root.last_child().unwrap().text().unwrap())
        .unwrap();
    let other = common::other_key().to_public_key();
    assert!(leafport_core::signing::verify(root, &other, &signature).is_err());

    let altered = signed.replace("<user>u</user>", "<user>x</user>");
    let document = roxmltree::Document::parse(&altered).unwrap();
    let public = common::key().to_public_key();
    assert!(leafport_core::signing::verify(document.root_element(), &public, &signature).is_err());
}

#[test]
fn nested_signatures_are_preserved_and_root_signatures_rejected() {
    let nested =
        r#"<req xmlns="http://ns.adobe.com/adept"><token><signature>T</signature></token></req>"#;
    let signed = sign_request(nested, common::key()).unwrap();
    assert!(signed.contains("<token><signature>T</signature></token>"));
    common::verify_signed(&signed, common::key());
    assert!(matches!(
        sign_request(&signed, common::key()),
        Err(RequestSigningError::AlreadySigned)
    ));
}

#[test]
fn oversized_dtd_and_malformed_input_are_rejected() {
    let big = format!("<a>{}</a>", "x".repeat(4 * 1024 * 1024));
    assert!(matches!(
        sign_request(&big, common::key()),
        Err(RequestSigningError::TooLarge)
    ));
    assert!(matches!(
        sign_request(r#"<!DOCTYPE a [<!ENTITY e "x">]><a>&e;</a>"#, common::key()),
        Err(RequestSigningError::Xml(_))
    ));
    assert!(matches!(
        sign_request("<a>", common::key()),
        Err(RequestSigningError::Xml(_))
    ));
}
