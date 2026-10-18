use leafport_core::infoset::{digest, encode};

fn encoded(xml: &str) -> Vec<u8> {
    let document = roxmltree::Document::parse(xml).unwrap();
    encode(document.root_element())
}

fn string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

#[test]
fn element_attribute_and_text_use_tagged_length_prefixed_records() {
    let mut expected = vec![1];
    string(&mut expected, "urn:x");
    string(&mut expected, "a");
    expected.push(5);
    string(&mut expected, "");
    string(&mut expected, "k");
    string(&mut expected, "v");
    expected.push(2);
    expected.push(4);
    string(&mut expected, "hi");
    expected.push(3);
    assert_eq!(encoded(r#"<a xmlns="urn:x" k="v">hi</a>"#), expected);
}

#[test]
fn text_is_trimmed_and_whitespace_only_text_is_dropped() {
    assert_eq!(encoded("<a>  hi\n</a>"), encoded("<a>hi</a>"));
    assert_eq!(encoded("<a>\n  <b/>\n</a>"), encoded("<a><b/></a>"));
}

#[test]
fn attributes_are_sorted_by_namespace_then_name() {
    assert_eq!(
        encoded(r#"<a xmlns:p="urn:p" z="1" p:a="2" b="3"/>"#),
        encoded(r#"<a xmlns:p="urn:p" b="3" z="1" p:a="2"/>"#),
    );
}

#[test]
fn namespace_declarations_and_prefixes_do_not_change_the_encoding() {
    assert_eq!(
        encoded(r#"<p:a xmlns:p="http://ns.adobe.com/adept"><p:b>x</p:b></p:a>"#),
        encoded(r#"<a xmlns="http://ns.adobe.com/adept"><b>x</b></a>"#),
    );
}

#[test]
fn adept_signature_and_hmac_are_excluded_but_foreign_ones_are_not() {
    let plain = r#"<a xmlns="http://ns.adobe.com/adept"><user>u</user></a>"#;
    let signed = r#"<a xmlns="http://ns.adobe.com/adept"><user>u</user><signature>AAAA</signature><hmac>BB</hmac></a>"#;
    assert_eq!(encoded(plain), encoded(signed));
    let foreign = r#"<a xmlns="http://ns.adobe.com/adept"><user>u</user><signature xmlns="urn:other">AAAA</signature></a>"#;
    assert_ne!(encoded(plain), encoded(foreign));
}

#[test]
fn comments_and_processing_instructions_are_ignored() {
    assert_eq!(encoded("<a><!-- c --><?pi x?>t</a>"), encoded("<a>t</a>"));
}

#[test]
fn digest_is_sha1_of_the_encoding() {
    use sha1::{Digest, Sha1};
    let xml = r#"<a xmlns="urn:x" k="v"><b>hi</b></a>"#;
    let document = roxmltree::Document::parse(xml).unwrap();
    let expected: [u8; 20] = Sha1::digest(encoded(xml)).into();
    assert_eq!(digest(document.root_element()), expected);
}
