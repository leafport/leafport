//! ADEPT InfoSet byte encoding, hashed to sign requests.
//!
//! This is not W3C XML canonicalization. Live signature parity is unverified.

const ADEPT: &str = "http://ns.adobe.com/adept";

/// Digest used by signed ADEPT requests: SHA-1.
pub fn digest(element: roxmltree::Node<'_, '_>) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    Sha1::digest(encode(element)).into()
}

/// Encode an element and its descendants for the ADEPT signing digest.
pub fn encode(element: roxmltree::Node<'_, '_>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.visit(element);
    encoder.bytes
}

#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
    text: String,
}

impl Encoder {
    fn string(&mut self, value: &str) {
        // Emit the low 16 bits of the byte length, then all bytes.
        self.bytes
            .extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.bytes.extend_from_slice(value.as_bytes());
    }

    fn flush_text(&mut self) {
        // Trim C whitespace (isspace) from both ends of the buffer.
        let text = std::mem::take(&mut self.text);
        let trimmed =
            text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{b}' | '\u{c}'));
        if !trimmed.is_empty() {
            self.bytes.push(4);
            self.string(trimmed);
        }
    }

    fn visit(&mut self, node: roxmltree::Node<'_, '_>) {
        if node.is_text() {
            self.text.push_str(node.text().unwrap_or(""));
            return;
        }
        if !node.is_element() {
            return;
        }
        let tag = node.tag_name();
        if tag.namespace() == Some(ADEPT) && matches!(tag.name(), "signature" | "hmac") {
            return;
        }
        self.flush_text();
        self.bytes.push(1);
        self.string(tag.namespace().unwrap_or(""));
        self.string(tag.name());
        let mut attributes: Vec<_> = node.attributes().collect();
        attributes.sort_by(|left, right| {
            (left.namespace().unwrap_or(""), left.name())
                .cmp(&(right.namespace().unwrap_or(""), right.name()))
        });
        for attribute in attributes {
            // roxmltree represents namespace declarations separately.
            self.bytes.push(5);
            self.string(attribute.namespace().unwrap_or(""));
            self.string(attribute.name());
            self.string(attribute.value());
        }
        self.bytes.push(2);
        for child in node.children() {
            self.visit(child);
        }
        self.flush_text();
        self.bytes.push(3);
    }
}
