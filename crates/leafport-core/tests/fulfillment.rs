mod common;

use leafport_core::fulfillment::{
    Acsm, AcsmError, ActivationToken, DeviceDescriptor, DeviceRole, FulfillResponseError,
    MAX_ACSM_BYTES, fulfill_url, loan_return_request, loan_return_url, parse_fulfillment,
    perform_fulfillment, require_web_url,
};

fn acsm_with_operator(url: &str) -> String {
    format!(
        r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept"><operatorURL>{url}</operatorURL></fulfillmentToken>"#
    )
}

#[test]
fn acsm_rejects_bad_operator_urls() {
    for url in [
        "https://user:pw@acs.example/fulfillment",
        "https://user@acs.example/fulfillment",
        "https://acs.example/fulfillment#frag",
        "ftp://acs.example/",
        "not a url",
    ] {
        assert!(
            matches!(
                Acsm::parse(acsm_with_operator(url)),
                Err(AcsmError::BadOperatorUrl)
            ),
            "{url}"
        );
    }
}

#[test]
fn require_web_url_accepts_http_and_https() {
    assert!(require_web_url("https://acs.example/fulfillment").is_ok());
    assert!(require_web_url("http://acs.example/").is_ok());
    assert!(require_web_url("ftp://acs.example/").is_err());
}

#[test]
fn acsm_accepts_plain_http_operator_url() {
    let acsm = Acsm::parse(acsm_with_operator(
        "http://play.google.com/books/volumes/x/acsm",
    ))
    .unwrap();
    assert_eq!(
        acsm.operator_url(),
        "http://play.google.com/books/volumes/x/acsm"
    );
}

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
fn acsm_keeps_token_bytes_and_trims_operator_url() {
    let acsm = Acsm::parse(ACSM.to_owned()).unwrap();
    assert!(acsm.token_xml().starts_with("<fulfillmentToken "));
    assert!(acsm.token_xml().ends_with("</fulfillmentToken>"));
    assert_eq!(acsm.document_xml(), ACSM);
    assert_eq!(acsm.operator_url(), "https://acs.example/fulfillment");
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
        Some(
            r#"<licenseToken xmlns="http://ns.adobe.com/adept"><encryptedKey>K</encryptedKey></licenseToken>"#
        )
    );
    let token = result.license_token_xml.unwrap();
    let document = roxmltree::Document::parse(&token).unwrap();
    let key = document
        .descendants()
        .find(|node| node.tag_name().name() == "encryptedKey")
        .unwrap();
    assert_eq!(
        key.tag_name().namespace(),
        Some("http://ns.adobe.com/adept")
    );
    assert!(result.loan_token_xml.is_none());
}

#[test]
fn prefixed_license_token_parses_standalone() {
    let xml = r#"<a:r xmlns:a="http://ns.adobe.com/adept" xmlns:x="urn:x"><a:src>https://h/b</a:src>
        <a:licenseToken x:k="v"><a:encryptedKey>K</a:encryptedKey></a:licenseToken></a:r>"#;
    let token = parse_fulfillment(xml).unwrap().license_token_xml.unwrap();
    assert!(token.starts_with("<a:licenseToken xmlns:a=\"http://ns.adobe.com/adept\""));
    let document = roxmltree::Document::parse(&token).unwrap();
    let root = document.root_element();
    assert!(root.has_tag_name(("http://ns.adobe.com/adept", "licenseToken")));
    assert_eq!(root.attribute(("urn:x", "k")), Some("v"));
    assert!(
        root.children()
            .any(|node| node.has_tag_name(("http://ns.adobe.com/adept", "encryptedKey")))
    );
}

#[test]
fn fulfillment_response_extracts_loan_token() {
    let xml = r#"<r xmlns="http://ns.adobe.com/adept"><src>https://h/b</src>
        <loanToken xmlns="http://ns.adobe.com/adept"><loan>L</loan></loanToken></r>"#;
    let loan = parse_fulfillment(xml).unwrap().loan_token_xml.unwrap();
    // Already declared on the element, so not duplicated.
    assert_eq!(
        loan,
        r#"<loanToken xmlns="http://ns.adobe.com/adept"><loan>L</loan></loanToken>"#
    );
    roxmltree::Document::parse(&loan).unwrap();
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

#[test]
fn fulfillment_response_exposes_title_author_id_and_loan_expiry() {
    let xml = r#"<fulfillmentResult xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/">
      <fulfillment>F-1</fulfillment>
      <resourceItemInfo><resource>urn:uuid:book</resource>
        <metadata><dc:title>White Sand</dc:title><dc:creator>Brandon Sanderson</dc:creator></metadata>
        <src>https://dl.example/book.epub</src>
        <licenseToken><permissions>
          <display><until>2026-11-09T00:00:00Z</until></display>
          <play><until>2026-11-02T00:00:00Z</until></play>
        </permissions></licenseToken>
      </resourceItemInfo></fulfillmentResult>"#;
    let f = parse_fulfillment(xml).unwrap();
    assert_eq!(f.fulfillment_id.as_deref(), Some("F-1"));
    assert_eq!(f.title.as_deref(), Some("White Sand"));
    assert_eq!(f.author.as_deref(), Some("Brandon Sanderson"));
    assert_eq!(f.loan_expires.as_deref(), Some("2026-11-02T00:00:00Z"));
}

#[test]
fn fulfillment_service_error_display_carries_data() {
    let err = parse_fulfillment(
        r#"<error xmlns="http://ns.adobe.com/adept" data="E_GOOGLE_DEVICE_LIMIT_REACHED http://x"/>"#,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "E_ADEPT_FULFILL: E_GOOGLE_DEVICE_LIMIT_REACHED http://x"
    );
}

#[test]
fn perform_fulfillment_runs_over_a_fake_transport() {
    let acsm = Acsm::parse(acsm_with_operator("https://op.example/fulfillment")).unwrap();
    let ok = r#"<fulfillmentResult xmlns="http://ns.adobe.com/adept"><resourceItemInfo><resource>urn:uuid:b</resource><src>https://dl.example/b.epub</src></resourceItemInfo></fulfillmentResult>"#;
    let transport = common::FakeTransport::new()
        .respond("/fulfillment/Fulfill", 200, ok)
        .respond("/b.epub", 200, b"BOOK".to_vec());
    let book = perform_fulfillment(
        &transport,
        &acsm,
        "u",
        "d",
        "standalone",
        &[],
        common::key(),
    )
    .unwrap();
    assert_eq!(book.body, b"BOOK");
    assert_eq!(transport.requests.lock().unwrap().len(), 2);
}

#[test]
fn perform_fulfillment_downloads_the_book_through_transport_download() {
    use leafport_core::service::{AdeptResponse, ServiceError, Transport};
    use std::sync::Mutex;

    /// Delegates to the fake, recording which URLs went through `download`.
    struct Downloads {
        inner: common::FakeTransport,
        downloaded: Mutex<Vec<String>>,
    }
    impl Transport for Downloads {
        fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
            self.inner.post_adept(url, xml)
        }
        fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
            self.inner.fetch(url, max_bytes)
        }
        fn download(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
            self.downloaded.lock().unwrap().push(url.to_owned());
            self.inner.fetch(url, max_bytes)
        }
    }

    let acsm = Acsm::parse(acsm_with_operator("https://op.example/fulfillment")).unwrap();
    let ok = r#"<fulfillmentResult xmlns="http://ns.adobe.com/adept"><resourceItemInfo><resource>urn:uuid:b</resource><src>https://dl.example/b.epub</src></resourceItemInfo></fulfillmentResult>"#;
    let transport = Downloads {
        inner: common::FakeTransport::new()
            .respond("/fulfillment/Fulfill", 200, ok)
            .respond("/b.epub", 200, b"BOOK".to_vec()),
        downloaded: Mutex::new(Vec::new()),
    };
    let book = perform_fulfillment(
        &transport,
        &acsm,
        "u",
        "d",
        "standalone",
        &[],
        common::key(),
    )
    .unwrap();
    assert_eq!(book.body, b"BOOK");
    assert_eq!(
        *transport.downloaded.lock().unwrap(),
        ["https://dl.example/b.epub"]
    );
}

#[test]
fn perform_fulfillment_sends_nothing_when_before_fulfill_refuses() {
    use leafport_core::service::{AdeptResponse, ServiceError, Transport};

    struct Refusing(common::FakeTransport);
    impl Transport for Refusing {
        fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
            self.0.post_adept(url, xml)
        }
        fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
            self.0.fetch(url, max_bytes)
        }
        fn before_fulfill(&self) -> Result<(), ServiceError> {
            Err(ServiceError::Cancelled)
        }
    }

    let acsm = Acsm::parse(acsm_with_operator("https://op.example/fulfillment")).unwrap();
    let transport = Refusing(common::FakeTransport::new());
    let result = perform_fulfillment(
        &transport,
        &acsm,
        "u",
        "d",
        "standalone",
        &[],
        common::key(),
    );
    assert!(result.is_err());
    assert!(transport.0.requests.lock().unwrap().is_empty());
}

#[test]
fn a_failed_book_download_names_its_host_and_error_page() {
    let acsm = Acsm::parse(acsm_with_operator("https://op.example/fulfillment")).unwrap();
    let ok = r#"<fulfillmentResult xmlns="http://ns.adobe.com/adept"><resourceItemInfo><resource>urn:uuid:b</resource><src>https://dl.example/b.pdf?token=SECRET</src></resourceItemInfo></fulfillmentResult>"#;
    let page = "<!DOCTYPE html><html><head><title>Sorry...</title></head><body><p>We're sorry...\n  but your computer may be sending automated queries.</p></body></html>";
    let transport = common::FakeTransport::new()
        .respond("/fulfillment/Fulfill", 200, ok)
        .respond("/b.pdf?token=SECRET", 503, page);
    let Err(error) = perform_fulfillment(
        &transport,
        &acsm,
        "u",
        "d",
        "standalone",
        &[],
        common::key(),
    ) else {
        panic!("download succeeded");
    };
    let error = error.to_string();
    assert_eq!(
        error,
        "book download failed: HTTP 503 Service Unavailable from dl.example: Sorry... We're sorry... but your computer may be sending automated queries."
    );
    assert!(!error.contains("SECRET"));
}
