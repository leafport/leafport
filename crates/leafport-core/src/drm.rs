//! ADEPT EPUB content decryption.
//!
//! A fulfilled EPUB carries `META-INF/rights.xml` (the licenseToken, including the
//! content key wrapped to the device's license key) and `META-INF/encryption.xml`
//! (the XML-ENC manifest of encrypted resources). This module unwraps the content key
//! (RSA PKCS#1 v1.5) and decrypts a resource (AES-128-CBC), following the standard
//! ADEPT scheme.

use std::{error::Error, fmt};

use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

const ADEPT: &str = "http://ns.adobe.com/adept";
pub(crate) const XMLENC: &str = "http://www.w3.org/2001/04/xmlenc#";
const AES128_CBC: &str = "http://www.w3.org/2001/04/xmlenc#aes128-cbc";

/// Upper bound on an inflated resource, guarding against deflate bombs.
pub const MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum DrmError {
    Xml(roxmltree::Error),
    /// rights.xml had no encryptedKey element.
    MissingKey,
    /// The wrapped key did not Base64-decode.
    KeyEncoding(base64::DecodeError),
    /// RSA unwrap of the content key failed.
    Unwrap(rsa::errors::Error),
    /// The unwrapped content key was not 16 bytes.
    KeyLength(usize),
    /// The encrypted resource was shorter than one AES block plus its IV.
    Truncated,
    /// Ciphertext length was not a multiple of the AES block size.
    Misaligned,
    /// Raw-inflate of the decrypted stream failed.
    Inflate(std::io::Error),
    /// The inflated resource exceeded MAX_RESOURCE_BYTES.
    TooLarge,
}

impl fmt::Display for DrmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(error) => write!(f, "invalid DRM metadata XML: {error}"),
            Self::MissingKey => write!(f, "rights.xml has no encryptedKey"),
            Self::KeyEncoding(error) => write!(f, "encryptedKey is not valid Base64: {error}"),
            Self::Unwrap(error) => write!(f, "content key unwrap failed: {error}"),
            Self::KeyLength(length) => {
                write!(f, "content key is {length} bytes, expected 16")
            }
            Self::Truncated => write!(f, "encrypted resource is too short to contain an IV"),
            Self::Misaligned => write!(f, "encrypted resource is not block-aligned"),
            Self::Inflate(error) => write!(f, "decrypted stream did not inflate: {error}"),
            Self::TooLarge => write!(f, "inflated resource exceeds {MAX_RESOURCE_BYTES} bytes"),
        }
    }
}
impl Error for DrmError {}

/// Unwrap the 16-byte AES content key from rights.xml using the device license key.
/// The encryptedKey is the content key RSA-encrypted (PKCS#1 v1.5) to the license key.
pub fn content_key(rights_xml: &str, license_key: &RsaPrivateKey) -> Result<[u8; 16], DrmError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let document = roxmltree::Document::parse_with_options(
        rights_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(DrmError::Xml)?;
    let encoded: String = document
        .descendants()
        .find(|node| node.has_tag_name((ADEPT, "encryptedKey")))
        .ok_or(DrmError::MissingKey)?
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let wrapped = STANDARD.decode(encoded).map_err(DrmError::KeyEncoding)?;
    let key = license_key
        .decrypt(Pkcs1v15Encrypt, &wrapped)
        .map_err(DrmError::Unwrap)?;
    key.as_slice()
        .try_into()
        .map_err(|_| DrmError::KeyLength(key.len()))
}

/// One encrypted-resource record from encryption.xml: the archive path and whether the
/// plaintext is raw-deflate compressed (ADEPT Compression Method="8").
#[derive(Clone, Debug)]
pub struct EncryptedResource {
    pub uri: String,
    pub compressed: bool,
}

fn parse_encryption(encryption_xml: &str) -> Result<roxmltree::Document<'_>, DrmError> {
    roxmltree::Document::parse_with_options(
        encryption_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            entity_resolver: None,
        },
    )
    .map_err(DrmError::Xml)
}

/// Whether an EncryptedData entry is ADEPT AES-128-CBC. A missing EncryptionMethod is
/// the ADEPT default; other algorithms (IDPF font obfuscation, Adobe PDF RC) are not.
pub(crate) fn is_adept_data(data: roxmltree::Node<'_, '_>) -> bool {
    data.children()
        .find(|node| node.has_tag_name((XMLENC, "EncryptionMethod")))
        .is_none_or(|method| method.attribute("Algorithm") == Some(AES128_CBC))
}

/// Whether encryption.xml lists any ADEPT-encrypted resource, i.e. the book is
/// DRM-protected. Unparseable XML is treated as not ADEPT.
pub fn is_adept_encryption(encryption_xml: &str) -> bool {
    parse_encryption(encryption_xml).is_ok_and(|document| {
        document
            .descendants()
            .filter(|node| node.has_tag_name((XMLENC, "EncryptedData")))
            .any(is_adept_data)
    })
}

/// Parse encryption.xml into the set of ADEPT-encrypted resources. URIs are
/// percent-decoded by the caller against the archive; they are returned here as written.
pub fn encrypted_resources(encryption_xml: &str) -> Result<Vec<EncryptedResource>, DrmError> {
    let document = parse_encryption(encryption_xml)?;
    let mut resources = Vec::new();
    for data in document
        .descendants()
        .filter(|node| node.has_tag_name((XMLENC, "EncryptedData")))
        .filter(|node| is_adept_data(*node))
    {
        let Some(uri) = data
            .descendants()
            .find(|node| node.has_tag_name((XMLENC, "CipherReference")))
            .and_then(|node| node.attribute("URI"))
        else {
            continue;
        };
        // ADEPT marks deflate-before-encrypt with an adept:Compression Method of 8.
        let compressed = data
            .descendants()
            .find(|node| node.has_tag_name((ADEPT, "Compression")))
            .and_then(|node| node.attribute("Method"))
            .is_some_and(|method| method == "8");
        resources.push(EncryptedResource {
            uri: uri.to_owned(),
            compressed,
        });
    }
    Ok(resources)
}

/// Decrypt one encrypted resource. ADEPT prepends a 16-byte IV to the AES-128-CBC
/// ciphertext; when the resource was compressed, the plaintext is raw-deflate and is
/// inflated here, capped at MAX_RESOURCE_BYTES. A compressed stream self-terminates, so
/// its padding is ignored; an uncompressed resource has valid PKCS#7 padding stripped
/// (trailing pad bytes would break XML parsing) and is otherwise returned as-is.
pub fn decrypt_resource(
    data: &[u8],
    key: &[u8; 16],
    compressed: bool,
) -> Result<Vec<u8>, DrmError> {
    use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
    if data.len() < 32 {
        return Err(DrmError::Truncated);
    }
    let (iv, ciphertext) = data.split_at(16);
    if ciphertext.len() % 16 != 0 {
        return Err(DrmError::Misaligned);
    }
    let mut buffer = ciphertext.to_vec();
    let plain = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
        .map_err(|_| DrmError::Misaligned)?
        .decrypt_padded::<NoPadding>(&mut buffer)
        .map_err(|_| DrmError::Misaligned)?;
    if !compressed {
        let pad = usize::from(*plain.last().unwrap_or(&0));
        let padded = (1..=16).contains(&pad)
            && pad <= plain.len()
            && plain[plain.len() - pad..]
                .iter()
                .all(|&b| usize::from(b) == pad);
        let end = if padded {
            plain.len() - pad
        } else {
            plain.len()
        };
        return Ok(plain[..end].to_vec());
    }
    use std::io::Read;
    let mut output = Vec::with_capacity(plain.len() * 3);
    flate2::read::DeflateDecoder::new(plain)
        .take(MAX_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut output)
        .map_err(DrmError::Inflate)?;
    if output.len() > MAX_RESOURCE_BYTES {
        return Err(DrmError::TooLarge);
    }
    Ok(output)
}
