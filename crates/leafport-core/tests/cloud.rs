mod common;

use leafport_core::cloud::{
    CloudClient, CloudError, CloudResponseError, PRODUCTION_BASE, acknowledge_upload,
    download_records, download_request, sync_to_cloud, sync_to_device, upload_request,
};
use leafport_core::signing::RequestSigningError;

#[test]
fn download_request_omits_missing_and_empty_dates() {
    let request = download_request("u&1", Some(""), Some("2026-01-01"), common::key()).unwrap();
    assert!(request.starts_with(
        r#"<synctodevice xmlns="http://ns.adobe.com/adept"><user>u&amp;1</user><toDate>2026-01-01</toDate><signature"#
    ));
    common::verify_signed(&request, common::key());
}

#[test]
fn upload_request_base64_encodes_acsm_and_bounds_size() {
    let request = upload_request("u", b"<acsm/>", common::key()).unwrap();
    assert!(request.contains("<acsm>PGFjc20vPg==</acsm>"));
    common::verify_signed(&request, common::key());
    assert!(matches!(
        upload_request("u", &vec![0; 3 * 1024 * 1024 + 1], common::key()),
        Err(RequestSigningError::TooLarge)
    ));
}

#[test]
fn download_records_decode_and_skip_incomplete_entries() {
    let xml = r#"<synctodeviceResult xmlns="http://ns.adobe.com/adept">
        <user2acsm><user>a</user><acsm>PGFj
            c20vPg==</acsm><extra/></user2acsm>
        <user2acsm><user>b</user></user2acsm>
        <other><user2acsm><user>c</user><acsm>eA==</acsm></user2acsm></other>
    </synctodeviceResult>"#;
    let records = download_records(xml).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].user, "a");
    assert_eq!(records[0].acsm, b"<acsm/>");
}

#[test]
fn download_records_report_service_errors_and_skip_bad_base64() {
    assert!(download_records("<empty/>").unwrap().is_empty());
    assert!(matches!(
        download_records(r#"<error xmlns="http://ns.adobe.com/adept"/>"#),
        Err(CloudResponseError::Service { .. })
    ));
    let records = download_records(
        r#"<synctodeviceResult xmlns="http://ns.adobe.com/adept"><user2acsm><user>a</user><acsm>!</acsm></user2acsm><user2acsm><user>b</user><acsm>eA==</acsm></user2acsm></synctodeviceResult>"#,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].user, "b");
    assert_eq!(records[0].acsm, b"x");
}

#[test]
fn upload_acknowledgment_requires_explicit_success() {
    acknowledge_upload(
        r#"<synctocloudResult xmlns="http://ns.adobe.com/adept">SUCCESS</synctocloudResult>"#,
    )
    .unwrap();
    acknowledge_upload(
        "<synctocloudResult xmlns=\"http://ns.adobe.com/adept\">\n  SUCCESS\n</synctocloudResult>",
    )
    .unwrap();
    for xml in [
        r#"<synctocloudResult xmlns="http://ns.adobe.com/adept">FAILURE</synctocloudResult>"#,
        r#"<synctocloudResult>SUCCESS</synctocloudResult>"#,
        r#"<ok xmlns="http://ns.adobe.com/adept">SUCCESS</ok>"#,
    ] {
        assert!(matches!(
            acknowledge_upload(xml),
            Err(CloudResponseError::InvalidResponse)
        ));
    }
    assert!(matches!(
        acknowledge_upload(r#"<error xmlns="http://ns.adobe.com/adept"/>"#),
        Err(CloudResponseError::Service { .. })
    ));
}

#[test]
fn cloud_client_accepts_only_plain_http_bases() {
    assert!(CloudClient::new(leafport_core::cloud::PRODUCTION_BASE).is_ok());
    for base in [
        "ftp://h/x",
        "https://user:pw@h/x",
        "https://h/x?q=1",
        "https://h/x#f",
        "not a url",
    ] {
        assert!(
            matches!(CloudClient::new(base), Err(CloudError::Endpoint)),
            "{base}"
        );
    }
}

#[test]
fn sync_to_device_returns_raw_xml_and_records() {
    let body = r#"<synctodeviceResult xmlns="http://ns.adobe.com/adept"><user2acsm><user>a</user><acsm>PGFjc20vPg==</acsm></user2acsm></synctodeviceResult>"#;
    let transport = common::FakeTransport::new().respond("/ebookcloud/SyncToDevice", 200, body);
    let response =
        sync_to_device(&transport, PRODUCTION_BASE, "urn:uuid:u", common::key()).unwrap();
    assert_eq!(response.xml, body);
    assert_eq!(response.records.len(), 1);
}

#[test]
fn sync_to_cloud_requires_success_ack() {
    let ok = common::FakeTransport::new().respond(
        "/SyncToCloud",
        200,
        r#"<synctocloudResult xmlns="http://ns.adobe.com/adept">SUCCESS</synctocloudResult>"#,
    );
    assert!(sync_to_cloud(&ok, PRODUCTION_BASE, "u", b"<acsm/>", common::key()).is_ok());
    let bad = common::FakeTransport::new().respond(
        "/SyncToCloud",
        500,
        r#"<error xmlns="http://ns.adobe.com/adept" data="E_X"/>"#,
    );
    assert!(
        sync_to_cloud(&bad, PRODUCTION_BASE, "u", b"<acsm/>", common::key())
            .unwrap_err()
            .to_string()
            .contains("E_X")
    );
}
