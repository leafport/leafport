use leafport_core::vendors::VendorCatalog;

const FIXTURE: &str = include_str!("data/authentication-service-info.xml");

#[test]
fn dynamic_vendors_keep_distinct_ids_and_response_order() {
    let mut catalog = VendorCatalog::new();
    assert!(!catalog.is_loaded());
    catalog.apply_service_info(FIXTURE).unwrap();
    let entries = catalog.entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].method, "AdobeID");
    assert_eq!(entries[0].name, "Adobe ID");
    assert_eq!(entries[0].node_id, None);
    assert_eq!(entries[1].method, "VendorID");
    assert_eq!(entries[1].node_id.as_deref(), Some("urn:uuid:test-one"));
    assert_eq!(entries[1].name, "Bibliothèque & Books");
    assert_eq!(entries[2].node_id.as_deref(), Some("urn:uuid:test-two"));
    assert_eq!(entries[2].name, "Bücher");
    assert!(catalog.is_loaded());
}

#[test]
fn prefixes_and_missing_optional_id_do_not_lose_vendors() {
    let mut catalog = VendorCatalog::new();
    catalog.apply_service_info(r#"<a:authenticationServiceInfo xmlns:a="http://ns.adobe.com/adept"><a:signInMethods><a:signInMethod method="Custom"><![CDATA[Name & More]]></a:signInMethod></a:signInMethods></a:authenticationServiceInfo>"#).unwrap();
    assert_eq!(catalog.entries()[1].method, "Custom");
    assert_eq!(catalog.entries()[1].node_id, None);
    assert_eq!(catalog.entries()[1].name, "Name & More");
}

#[test]
fn unrelated_namespaces_and_nested_lists_are_not_vendor_entries() {
    let mut catalog = VendorCatalog::new();
    catalog.apply_service_info(r#"<root xmlns="http://ns.adobe.com/adept" xmlns:x="urn:other"><nested><signInMethods><signInMethod method="Wrong"/></signInMethods></nested><signInMethods><x:signInMethod method="Wrong"/><signInMethod method="Right">Right</signInMethod></signInMethods></root>"#).unwrap();
    assert_eq!(catalog.entries().len(), 2);
    assert_eq!(catalog.entries()[1].method, "Right");
}

#[test]
fn empty_response_preserves_builtin_choice_and_marks_loaded() {
    let mut catalog = VendorCatalog::new();
    catalog
        .apply_service_info("<root><signInMethods/></root>")
        .unwrap();
    assert_eq!(catalog.entries().len(), 1);
    assert!(catalog.is_loaded());
}

#[test]
fn malformed_response_cannot_replace_a_working_catalog() {
    let mut catalog = VendorCatalog::new();
    catalog.apply_service_info(FIXTURE).unwrap();
    let before = catalog.entries().to_vec();
    assert!(catalog.apply_service_info("<root><signInMethods>").is_err());
    assert_eq!(catalog.entries(), before);
    assert!(catalog.is_loaded());
}

#[test]
fn duplicates_and_repeated_loads_follow_native_append_behavior() {
    let mut catalog = VendorCatalog::new();
    let xml = r#"<root><signInMethods><signInMethod method="VendorID" nodeId="same">One</signInMethod><signInMethod method="VendorID" nodeId="same">Two</signInMethod></signInMethods></root>"#;
    catalog.apply_service_info(xml).unwrap();
    catalog.apply_service_info(xml).unwrap();
    assert_eq!(catalog.entries().len(), 5);
    assert_eq!(catalog.entries()[1].name, "One");
    assert_eq!(catalog.entries()[2].name, "Two");
    assert_eq!(catalog.entries()[3].name, "One");
}

#[test]
fn external_entities_and_oversized_input_fail_without_loading() {
    let mut catalog = VendorCatalog::new();
    assert!(catalog.apply_service_info(r#"<!DOCTYPE root [<!ENTITY secret SYSTEM "file:///etc/passwd">]><root>&secret;</root>"#).is_err());
    assert!(
        catalog
            .apply_service_info(&" ".repeat(4 * 1024 * 1024 + 1))
            .is_err()
    );
    assert!(!catalog.is_loaded());
    assert_eq!(catalog.entries().len(), 1);
}
