//! Legacy ADEPT request signing. Activation key acquisition is separate.
//!
//! The private RSA operation signs the SHA-1 digest with PKCS#1 v1.5 padding
//! directly, without an ASN.1 DigestInfo prefix. Live parity remains unverified.

use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use std::{error::Error, fmt};

#[derive(Debug)]
pub enum RequestSigningError {
    TooLarge,
    Xml(roxmltree::Error),
    AlreadySigned,
    Crypto(rsa::errors::Error),
}

impl fmt::Display for RequestSigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "request exceeds the local signing size limit"),
            Self::Xml(error) => write!(f, "invalid request XML: {error}"),
            Self::AlreadySigned => write!(f, "request already contains a signature"),
            Self::Crypto(error) => write!(f, "request signing failed: {error}"),
        }
    }
}

impl Error for RequestSigningError {}

/// Append an ADEPT signature to a fresh request without reserializing its token.
/// Signed nested tokens are preserved; only an existing root signature is rejected.
pub fn sign_request(xml: &str, key: &RsaPrivateKey) -> Result<String, RequestSigningError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    if xml.len() > 4 * 1024 * 1024 {
        return Err(RequestSigningError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(RequestSigningError::Xml)?;
    let root = document.root_element();
    if root
        .children()
        .any(|node| node.has_tag_name(("http://ns.adobe.com/adept", "signature")))
    {
        return Err(RequestSigningError::AlreadySigned);
    }
    let signature = STANDARD.encode(sign(root, key).map_err(RequestSigningError::Crypto)?);
    let element = format!("<signature xmlns=\"http://ns.adobe.com/adept\">{signature}</signature>");
    let range = root.range();
    let original = &xml[range.clone()];
    let mut result = String::with_capacity(xml.len() + element.len() + 64);
    if original.ends_with("/>") {
        let name_end = original[1..]
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .map(|offset| offset + 1)
            .unwrap_or(original.len());
        let name = &original[1..name_end];
        result.push_str(&xml[..range.end - 2]);
        result.push('>');
        result.push_str(&element);
        result.push_str("</");
        result.push_str(name);
        result.push('>');
        result.push_str(&xml[range.end..]);
    } else {
        // A parsed nonempty element necessarily ends with its own closing tag.
        let insertion = range.start + original.rfind("</").expect("parsed closing tag");
        result.push_str(&xml[..insertion]);
        result.push_str(&element);
        result.push_str(&xml[insertion..]);
    }
    Ok(result)
}

/// Sign a request element using the user's activation signing key.
/// OS randomness blinds the private operation; the signature is deterministic.
pub fn sign(
    element: roxmltree::Node<'_, '_>,
    key: &RsaPrivateKey,
) -> Result<Vec<u8>, rsa::errors::Error> {
    key.sign_with_rng(
        &mut rsa::rand_core::OsRng,
        Pkcs1v15Sign::new_unprefixed(),
        &crate::infoset::digest(element),
    )
}

/// Verify the raw signature bytes, using the same ADEPT infoset exclusions.
pub fn verify(
    element: roxmltree::Node<'_, '_>,
    key: &RsaPublicKey,
    signature: &[u8],
) -> Result<(), rsa::errors::Error> {
    key.verify(
        Pkcs1v15Sign::new_unprefixed(),
        &crate::infoset::digest(element),
        signature,
    )
}
