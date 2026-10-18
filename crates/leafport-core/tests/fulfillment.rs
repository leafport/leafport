mod common;

use leafport_core::fulfillment::{
    Acsm, AcsmError, ActivationToken, DeviceDescriptor, DeviceRole, FulfillResponseError,
    MAX_ACSM_BYTES, fulfill_url, loan_return_request, loan_return_url, parse_fulfillment,
};

const ACSM: &str = r#"<?xml version="1.0"?>
<fulfillmentToken fulfillmentType="buy" auth="user" xmlns="http://ns.adobe.com/adept" xmlns:request="urn:clash">
  <distributor>urn:uuid:d</distributor>
  <operatorURL> https://acs.example/fulfillment </operatorURL>
  <transaction>T-1</transaction>
  <hmac>AAAA</hmac>
</fulfillmentToken>"#;

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor {
        role: DeviceRole::Target,
        software_version: Some("1.0".into()),
        client_os: Some("Linux".into()),
        client_locale: None,
        client_version: Some("4.5".into()),
        device_type: "standalone".into(),
        product_name: None,
        device_name: Some("desk & top".into()),
        fingerprint: "FP==".into(),
        activations: vec![ActivationToken {
            user: "urn:uuid:u".into(),
            device: "urn:uuid:dev".into(),
            until: None,
            auth: Some("A".into()),
            auth_distributor: None,
        }],
    }
}

#[test]
fn acsm_keeps_token_bytes_and_untrimmed_operator_text() {
    let acsm = Acsm::parse(ACSM.to_owned()).unwrap();
    assert!(acsm.token_xml().starts_with("<fulfillmentToken "));
    assert!(acsm.token_xml().ends_with("</fulfillmentToken>"));
    assert_eq!(acsm.document_xml(), ACSM);
    assert_eq!(acsm.operator_url(), " https://acs.example/fulfillment ");
}

#[test]
fn acsm_errors_are_specific() {
    assert!(matches!(
        Acsm::parse("<a/>".into()),
        Err(AcsmError::MissingToken)
    ));
    assert!(matches!(
        Acsm::parse(r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept"/>"#.into()),
        Err(AcsmError::MissingOperatorUrl)
    ));
    assert!(matches!(
        Acsm::parse(
            r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept"><operatorURL> </operatorURL></fulfillmentToken>"#
                .into()
        ),
        Err(AcsmError::MissingOperatorUrl)
    ));
    assert!(matches!(
        Acsm::parse("x".repeat(MAX_ACSM_BYTES + 1)),
        Err(AcsmError::TooLarge)
    ));
    assert!(matches!(Acsm::parse("<a".into()), Err(AcsmError::Xml(_))));
}

#[test]
fn operation_urls_join_with_a_single_slash() {
    assert_eq!(fulfill_url("https://h/f/"), "https://h/f/Fulfill");
    assert_eq!(fulfill_url("https://h/f"), "https://h/f/Fulfill");
    assert_eq!(loan_return_url("https://h/f//"), "https://h/f/LoanReturn");
}

#[test]
fn device_descriptor_follows_protocol_field_order() {
    assert_eq!(
        descriptor().to_xml(),
        concat!(
            r#"<targetDevice xmlns="http://ns.adobe.com/adept">"#,
            "<softwareVersion>1.0</softwareVersion><clientOS>Linux</clientOS>",
            "<clientVersion>4.5</clientVersion><deviceType>standalone</deviceType>",
            "<deviceName>desk &amp; top</deviceName><fingerprint>FP==</fingerprint>",
            "<activationToken><user>urn:uuid:u</user><device>urn:uuid:dev</device></activationToken>",
            "</targetDevice>"
        )
    );
}

#[test]
fn product_name_wins_over_device_name_and_auth_needs_distributor() {
    let mut device = descriptor();
    device.role = DeviceRole::Proxy;
    device.product_name = Some("P".into());
    device.activations[0].until = Some("2030".into());
    device.activations[0].auth_distributor = Some("D".into());
    let xml = device.to_xml();
    assert!(xml.starts_with("<proxyDevice "));
    assert!(xml.contains("<productName>P</productName>"));
    assert!(!xml.contains("deviceName"));
    assert!(xml.contains("<until>2030</until><auth>A</auth><authdistributor>D</authdistributor>"));
}

#[test]
fn signed_fulfill_request_embeds_token_and_avoids_prefix_clash() {
    let acsm = Acsm::parse(ACSM.to_owned()).unwrap();
    let request = acsm
        .signed_request(
            "urn:uuid:u",
            "urn:uuid:dev",
            "standalone",
            &[descriptor()],
            common::key(),
        )
        .unwrap();
    // The token binds `request`, so the envelope must use another prefix.
    assert!(request.starts_with(r#"<request_:fulfill xmlns:request_="http://ns.adobe.com/adept""#));
    assert!(request.contains(acsm.token_xml()));
    assert!(request.contains("<request_:user>urn:uuid:u</request_:user>"));
    common::verify_signed(&request, common::key());

    let document = roxmltree::Document::parse(&request).unwrap();
    let children: Vec<_> = document
        .root_element()
        .children()
        .filter(|node| node.is_element())
        .map(|node| node.tag_name().name())
        .collect();
    assert_eq!(
        children,
        [
            "user",
            "device",
            "deviceType",
            "fulfillmentToken",
            "targetDevice",
            "signature"
        ]
    );
}

#[test]
fn fulfillment_response_extracts_download_and_license() {
    let xml = r#"<envelope xmlns="http://ns.adobe.com/adept"><fulfillmentResult>
        <resourceItemInfo><resource>urn:uuid:book</resource>
        <licenseToken><encryptedKey>K</encryptedKey></licenseToken>
        <src> https://dl.example/book.epub </src><downloadType>application/epub+zip</downloadType>
        </resourceItemInfo></fulfillmentResult></envelope>"#;
    let result = parse_fulfillment(xml).unwrap();
    assert_eq!(result.download_url, "https://dl.example/book.epub");
    assert_eq!(result.resource.as_deref(), Some("urn:uuid:book"));
    assert_eq!(
        result.download_type.as_deref(),
        Some("application/epub+zip")
    );
    assert_eq!(
        result.license_token_xml.as_deref(),
        Some("<licenseToken><encryptedKey>K</encryptedKey></licenseToken>")
    );
}

#[test]
fn fulfillment_response_falls_back_to_href_and_reports_errors() {
    let href = r#"<r xmlns="http://ns.adobe.com/adept"><href>https://h/b</href></r>"#;
    assert_eq!(parse_fulfillment(href).unwrap().download_url, "https://h/b");
    assert!(parse_fulfillment(href).unwrap().license_token_xml.is_none());
    assert!(matches!(
        parse_fulfillment(r#"<r xmlns="http://ns.adobe.com/adept"><src> </src></r>"#),
        Err(FulfillResponseError::MissingDownload)
    ));
    let error = r#"<error xmlns="http://ns.adobe.com/adept" data="E_GOOGLE_ERROR"/>"#;
    match parse_fulfillment(error) {
        Err(FulfillResponseError::Service { xml }) => assert_eq!(xml, error),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn loan_return_request_is_signed_in_field_order() {
    let request = loan_return_request("u", "d", "<loanToken>L</loanToken>", common::key()).unwrap();
    assert!(request.starts_with(
        r#"<loanReturn xmlns="http://ns.adobe.com/adept"><user>u</user><device>d</device><loanToken>L</loanToken><signature"#
    ));
    common::verify_signed(&request, common::key());
}
