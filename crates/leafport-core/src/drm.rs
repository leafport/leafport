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
const XMLENC: &str = "http://www.w3.org/2001/04/xmlenc#";

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

/// Parse encryption.xml into the set of encrypted resources. URIs are percent-decoded
/// by the caller against the archive; they are returned here as written.
pub fn encrypted_resources(encryption_xml: &str) -> Result<Vec<EncryptedResource>, DrmError> {
    let document = roxmltree::Document::parse_with_options(
        encryption_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            entity_resolver: None,
        },
    )
    .map_err(DrmError::Xml)?;
    let mut resources = Vec::new();
    for data in document
        .descendants()
        .filter(|node| node.has_tag_name((XMLENC, "EncryptedData")))
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
/// inflated here. Block padding is not stripped explicitly: the inflate stream
/// self-terminates, and an uncompressed resource keeps any trailing padding, which the
/// XHTML parser tolerates.
pub fn decrypt_resource(
    data: &[u8],
    key: &[u8; 16],
    compressed: bool,
) -> Result<Vec<u8>, DrmError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
    if data.len() < 32 {
        return Err(DrmError::Truncated);
    }
    let (iv, ciphertext) = data.split_at(16);
    if ciphertext.len() % 16 != 0 {
        return Err(DrmError::Misaligned);
    }
    let mut buffer = ciphertext.to_vec();
    let plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut buffer)
        .map_err(|_| DrmError::Misaligned)?;
    if !compressed {
        return Ok(plain.to_vec());
    }
    use std::io::Read;
    let mut output = Vec::with_capacity(plain.len() * 3);
    flate2::read::DeflateDecoder::new(plain)
        .read_to_end(&mut output)
        .map_err(DrmError::Inflate)?;
    Ok(output)
}
