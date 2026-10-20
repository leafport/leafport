//! Adobe ID sign-in: key generation, encrypted credentials and the SignInDirect request.
//! Service certificate validation, private-key wrapping, and persistence are separate.

use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use std::{error::Error, fmt};

/// Wrap a private key for the service: a random IV followed by AES-128-CBC/PKCS#7
/// ciphertext. Input is the exported private key, whose DER container is handled separately.
pub fn wrap_private_key(device_key: &[u8; 16], exported_key: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    use rand::RngCore;
    let mut iv = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let ciphertext = cbc::Encryptor::<aes::Aes128>::new(device_key.into(), (&iv).into())
        .encrypt_padded_vec_mut::<Pkcs7>(exported_key);
    let mut wrapped = Vec::with_capacity(16 + ciphertext.len());
    wrapped.extend_from_slice(&iv);
    wrapped.extend_from_slice(&ciphertext);
    wrapped
}

/// Inverse of [`wrap_private_key`]: strip the IV prefix and AES-128-CBC/PKCS#7 decrypt.
pub fn unwrap_private_key(device_key: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    if wrapped.len() < 32 {
        return None;
    }
    let (iv, ciphertext) = wrapped.split_at(16);
    cbc::Decryptor::<aes::Aes128>::new(device_key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .ok()
}

/// The account's license key from a SignInDirect response. An existing Adobe ID keeps
/// its license key across devices: the service returns it as `encryptedPrivateLicenseKey`
/// wrapped with this device key, and licenses are encrypted to it rather than to the
/// locally generated key. Requests are signed with [`account_auth_key`] instead.
/// None when the response carries no usable key.
pub fn account_license_key(credentials_xml: &str, device_key: &[u8; 16]) -> Option<RsaPrivateKey> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use zeroize::Zeroize;
    let document = roxmltree::Document::parse_with_options(
        credentials_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .ok()?;
    let encoded: String = document
        .descendants()
        .find(|node| node.has_tag_name((ADEPT_NS, "encryptedPrivateLicenseKey")))?
        .text()?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let wrapped = STANDARD.decode(encoded).ok()?;
    let mut der = unwrap_private_key(device_key, &wrapped)?;
    let key = RsaPrivateKey::from_pkcs8_der(&der)
        .or_else(|_| RsaPrivateKey::from_pkcs1_der(&der))
        .ok();
    der.zeroize();
    key
}

/// The account's signing key from a SignInDirect response. ADEPT request signatures
/// are verified against the key in the returned `pkcs12`, whose password is the
/// Base64 device key (libgourou `signNode`); the license key is rejected with
/// E_ADEPT_INVALID_SIGNATURE (BadPadding). None when no usable key is present.
pub fn account_auth_key(credentials_xml: &str, device_key: &[u8; 16]) -> Option<RsaPrivateKey> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use zeroize::Zeroize;
    let document = roxmltree::Document::parse_with_options(
        credentials_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .ok()?;
    let encoded: String = document
        .descendants()
        .find(|node| node.has_tag_name((ADEPT_NS, "pkcs12")))?
        .text()?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let pfx = p12::PFX::parse(&STANDARD.decode(encoded).ok()?).ok()?;
    // PKCS#12 passwords are BMPStrings: UTF-16BE with a terminating NUL.
    let mut password: Vec<u8> = STANDARD
        .encode(device_key)
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_be_bytes)
        .collect();
    // Adobe's container is BER, and its RC2-40 certificate bag is not needed, so walk
    // the safe contents leniently instead of using `PFX::key_bags`, which rejects it.
    let mut keys = Vec::new();
    if let Some(safe) = pfx.auth_safe.data(&password) {
        let contents =
            yasna::parse_ber(&safe, |r| r.collect_sequence_of(p12::ContentInfo::parse))
                .unwrap_or_default();
        for content in &contents {
            let Some(data) = content.data(&password) else {
                continue;
            };
            let bags = yasna::parse_ber(&data, |r| r.collect_sequence_of(p12::SafeBag::parse))
                .unwrap_or_default();
            keys.extend(bags.iter().filter_map(|bag| bag.bag.get_key(&password)));
        }
    }
    password.zeroize();
    let key = keys.iter().find_map(|der| {
        RsaPrivateKey::from_pkcs8_der(der)
            .or_else(|_| RsaPrivateKey::from_pkcs1_der(der))
            .ok()
    });
    keys.iter_mut().for_each(|der| der.zeroize());
    key
}

pub struct ActivationKeys {
    pub authentication: RsaPrivateKey,
    pub license: RsaPrivateKey,
}

/// Operation appended to the discovered authURL.
pub const SIGN_IN_OPERATION: &str = "SignInDirect";

/// Build the unsigned signIn request in protocol field order:
/// signInData, publicAuthKey, encryptedPrivateAuthKey, publicLicenseKey,
/// encryptedPrivateLicenseKey. The service expects no ADEPT signature on it.
pub fn sign_in_request(
    method: &str,
    sign_in_data: &[u8],
    device_key: &[u8; 16],
    keys: &ActivationKeys,
) -> Result<String, KeyExportError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut xml = String::from("<signIn xmlns=\"http://ns.adobe.com/adept\" method=\"");
    for character in method.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '"' => xml.push_str("&quot;"),
            character => xml.push(character),
        }
    }
    xml.push_str("\">");
    crate::cloud::field(&mut xml, "signInData", &STANDARD.encode(sign_in_data));
    for (public_name, private_name, key) in [
        (
            "publicAuthKey",
            "encryptedPrivateAuthKey",
            &keys.authentication,
        ),
        (
            "publicLicenseKey",
            "encryptedPrivateLicenseKey",
            &keys.license,
        ),
    ] {
        let public = export_public_key(&key.to_public_key())?;
        crate::cloud::field(&mut xml, public_name, &STANDARD.encode(public));
        let mut private = export_private_key(key)?;
        let wrapped = wrap_private_key(device_key, &private);
        use zeroize::Zeroize;
        private.zeroize();
        crate::cloud::field(&mut xml, private_name, &STANDARD.encode(wrapped));
    }
    xml.push_str("</signIn>");
    Ok(xml)
}

#[derive(Debug)]
pub struct KeyExportError(String);

impl fmt::Display for KeyExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "activation key export failed: {}", self.0)
    }
}
impl Error for KeyExportError {}

/// Public keys export as DER SubjectPublicKeyInfo.
pub fn export_public_key(key: &RsaPublicKey) -> Result<Vec<u8>, KeyExportError> {
    use rsa::pkcs8::EncodePublicKey;
    key.to_public_key_der()
        .map(|der| der.as_bytes().to_vec())
        .map_err(|error| KeyExportError(error.to_string()))
}

/// Private keys export as unencrypted DER PKCS#8, wrapped afterward for the service.
pub fn export_private_key(key: &RsaPrivateKey) -> Result<Vec<u8>, KeyExportError> {
    use rsa::pkcs8::EncodePrivateKey;
    key.to_pkcs8_der()
        .map(|der| der.as_bytes().to_vec())
        .map_err(|error| KeyExportError(error.to_string()))
}

impl ActivationKeys {
    /// RSA keys as the service expects them: 1024 bits, public exponent 65537.
    pub fn generate() -> Result<Self, rsa::errors::Error> {
        let mut rng = rand::rngs::OsRng;
        Ok(Self {
            authentication: RsaPrivateKey::new(&mut rng, 1024)?,
            license: RsaPrivateKey::new(&mut rng, 1024)?,
        })
    }
}

#[derive(Debug)]
pub enum SignInDataError {
    CredentialTooLong,
    Crypto(rsa::errors::Error),
}

impl fmt::Display for SignInDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CredentialTooLong => write!(
                f,
                "sign-in credentials exceed the protocol byte-length limit"
            ),
            Self::Crypto(error) => write!(f, "sign-in encryption failed: {error}"),
        }
    }
}
impl Error for SignInDataError {}

/// Produce encrypted signInData. The caller supplies the service certificate's
/// public key and the device's existing 16-byte secret. No credentials are logged.
pub fn encrypt_sign_in_data(
    device_key: &[u8; 16],
    username: &str,
    password: &str,
    service_key: &RsaPublicKey,
) -> Result<Vec<u8>, SignInDataError> {
    if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
        return Err(SignInDataError::CredentialTooLong);
    }
    let mut plain = Vec::with_capacity(18 + username.len() + password.len());
    plain.extend_from_slice(device_key);
    plain.push(username.len() as u8);
    plain.extend_from_slice(username.as_bytes());
    plain.push(password.len() as u8);
    plain.extend_from_slice(password.as_bytes());
    let result = service_key
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &plain)
        .map_err(SignInDataError::Crypto);
    // The caller owns the original credentials. Wipe our temporary packed copy.
    use zeroize::Zeroize;
    plain.zeroize();
    result
}

/// Outcome of a SignInDirect exchange. The response carries an ADEPT `user` element
/// plus the signed-in credentials; an `error` root is a failure. The parser matches
/// ADEPT element names, as the cloud and fulfillment parsers do.
#[derive(Clone, Debug)]
pub struct SignInResult {
    /// The activated account/user identifier returned by the service.
    pub user: String,
    /// Credential material the service returns for the activated account, verbatim XML.
    /// It is retained untouched rather than reinterpreted here.
    pub credentials_xml: String,
}

#[derive(Debug)]
pub enum SignInResponseError {
    TooLarge,
    Xml(roxmltree::Error),
    /// ADEPT `error` root. The complete element is retained, not reduced to a code.
    Service {
        xml: String,
    },
    /// A SUCCESS-shaped response that is missing the account identifier.
    MissingUser,
}

impl fmt::Display for SignInResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "sign-in response exceeds the local size limit"),
            Self::Xml(error) => write!(f, "E_ADEPT_XML_SYNTAX: {error}"),
            Self::Service { .. } => write!(f, "E_AUTH_FAILED: service returned an ADEPT error"),
            Self::MissingUser => write!(f, "E_AUTH_BAD_SERVER_RESPONSE: response has no user"),
        }
    }
}
impl Error for SignInResponseError {}

const ADEPT_NS: &str = "http://ns.adobe.com/adept";

/// Parse a SignInDirect response. An ADEPT `error` root is a failure; otherwise the
/// account `user` and the returned credential subtree are retained for persistence.
pub fn parse_sign_in_response(xml: &str) -> Result<SignInResult, SignInResponseError> {
    if xml.len() > 4 * 1024 * 1024 {
        return Err(SignInResponseError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(SignInResponseError::Xml)?;
    let root = document.root_element();
    if root.has_tag_name((ADEPT_NS, "error")) {
        return Err(SignInResponseError::Service {
            xml: xml.to_owned(),
        });
    }
    let user = root
        .descendants()
        .find(|node| node.has_tag_name((ADEPT_NS, "user")))
        .and_then(|node| node.text())
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or(SignInResponseError::MissingUser)?;
    // Retain the response bytes as the credential record. Byte-faithful retention
    // keeps every returned credential without imposing a schema on it.
    Ok(SignInResult {
        user,
        credentials_xml: xml.to_owned(),
    })
}

/// Extract the RSA public key from the discovery `certificate` (Base64 DER X.509).
/// Sign-in encrypts signInData with the authentication service certificate's key.
/// This decodes the SubjectPublicKeyInfo; it makes no trust decision about the chain.
pub fn service_certificate_key(certificate: &str) -> Result<RsaPublicKey, KeyExportError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rsa::pkcs8::DecodePublicKey;
    use x509_cert::{Certificate, der::Decode, der::Encode};
    let der = STANDARD
        .decode(certificate.trim())
        .map_err(|error| KeyExportError(error.to_string()))?;
    let certificate =
        Certificate::from_der(&der).map_err(|error| KeyExportError(error.to_string()))?;
    let spki = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|error| KeyExportError(error.to_string()))?;
    RsaPublicKey::from_public_key_der(&spki).map_err(|error| KeyExportError(error.to_string()))
}

/// URL to post SignInDirect to: the discovered authURL joined with the operation.
pub fn sign_in_url(auth_url: &str) -> String {
    format!("{}/{}", auth_url.trim_end_matches('/'), SIGN_IN_OPERATION)
}

#[derive(Debug)]
pub enum ActivateError {
    /// The discovery certificate did not yield a usable RSA key.
    Certificate(KeyExportError),
    /// Packing/encrypting signInData failed.
    Data(SignInDataError),
    /// Building the signIn request (key export) failed.
    Build(KeyExportError),
    /// The HTTP exchange failed at the transport layer.
    Transport(crate::service::ServiceError),
    /// A non-success HTTP status. The body is retained for diagnosis.
    Http {
        status: reqwest::StatusCode,
        body: Vec<u8>,
    },
    /// The response body was not UTF-8.
    Utf8(std::string::FromUtf8Error),
    /// The response parsed but reported an ADEPT error or was malformed.
    Response(SignInResponseError),
}

impl fmt::Display for ActivateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Certificate(error) => error.fmt(f),
            Self::Data(error) => error.fmt(f),
            Self::Build(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Http { status, .. } => write!(f, "sign-in HTTP request failed: {status}"),
            Self::Utf8(error) => write!(f, "sign-in response is not UTF-8: {error}"),
            Self::Response(error) => error.fmt(f),
        }
    }
}
impl Error for ActivateError {}

/// Perform one SignInDirect exchange: encrypt the credentials against the discovery
/// certificate, build and POST the request, then parse the response. The caller owns
/// the device key and activation keys and persists the resulting state separately.
///
/// Credentials are RSA-encrypted before the request leaves this function and are never
/// logged. On an HTTP error the body is retained so a semantic ADEPT error can surface.
#[allow(clippy::too_many_arguments)]
pub fn perform_sign_in(
    client: &crate::service::ServiceClient,
    auth_url: &str,
    method: &str,
    username: &str,
    password: &str,
    certificate: &str,
    device_key: &[u8; 16],
    keys: &ActivationKeys,
) -> Result<SignInResult, ActivateError> {
    let service_key = service_certificate_key(certificate).map_err(ActivateError::Certificate)?;
    let sign_in_data = encrypt_sign_in_data(device_key, username, password, &service_key)
        .map_err(ActivateError::Data)?;
    let request =
        sign_in_request(method, &sign_in_data, device_key, keys).map_err(ActivateError::Build)?;
    let response = client
        .post_adept(&sign_in_url(auth_url), request)
        .map_err(ActivateError::Transport)?;
    if !response.status.is_success() {
        // Surface a semantic ADEPT error when the body carries one.
        if let Ok(xml) = std::str::from_utf8(&response.body)
            && let Err(error @ SignInResponseError::Service { .. }) = parse_sign_in_response(xml)
        {
            return Err(ActivateError::Response(error));
        }
        return Err(ActivateError::Http {
            status: response.status,
            body: response.body,
        });
    }
    let xml = String::from_utf8(response.body).map_err(ActivateError::Utf8)?;
    parse_sign_in_response(&xml).map_err(ActivateError::Response)
}

/// Generate a fresh 16-byte device key from OS randomness, persisted with the
/// activation state so the signed-in device stays stable across restarts.
pub fn generate_device_key() -> [u8; 16] {
    use rand::RngCore;
    let mut device_key = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut device_key);
    device_key
}

/// Derive the device identifier from the device key: an activation://device URN with a
/// UUID formed from the key bytes.
pub fn device_id(device_key: &[u8; 16]) -> String {
    let b = device_key;
    format!(
        "activation://device/{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// Derive the device fingerprint: Base64 of SHA-1 over the device key, the ADEPT
/// fingerprint format.
pub fn device_fingerprint(device_key: &[u8; 16]) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(device_key);
    STANDARD.encode(hasher.finalize())
}
