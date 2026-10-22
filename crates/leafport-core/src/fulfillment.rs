//! ACSM input for the fulfillment request.
//!
//! Transport and signing are not yet implemented here.

use std::{error::Error, fmt};

const ADEPT: &str = "http://ns.adobe.com/adept";
const DC: &str = "http://purl.org/dc/elements/1.1/";

/// Device role in a fulfill request.
#[derive(Clone, Copy, Debug)]
pub enum DeviceRole {
    Target,
    Proxy,
    Attached,
}

#[derive(Clone, Debug)]
pub struct ActivationToken {
    pub user: String,
    pub device: String,
    pub until: Option<String>,
    pub auth: Option<String>,
    pub auth_distributor: Option<String>,
}

/// Values come from device/activation state, not synthesized request defaults.
#[derive(Clone, Debug)]
pub struct DeviceDescriptor {
    pub role: DeviceRole,
    pub software_version: Option<String>,
    pub client_os: Option<String>,
    pub client_locale: Option<String>,
    pub client_version: Option<String>,
    pub device_type: String,
    pub product_name: Option<String>,
    pub device_name: Option<String>,
    pub fingerprint: String,
    pub activations: Vec<ActivationToken>,
}

impl DeviceDescriptor {
    /// Serialize the device portion of a fulfill request in protocol field order.
    pub fn to_xml(&self) -> String {
        let tag = match self.role {
            DeviceRole::Target => "targetDevice",
            DeviceRole::Proxy => "proxyDevice",
            DeviceRole::Attached => "attachedDevice",
        };
        let mut xml = format!("<{tag} xmlns=\"{ADEPT}\">");
        for (name, value) in [
            ("softwareVersion", self.software_version.as_deref()),
            ("clientOS", self.client_os.as_deref()),
            ("clientLocale", self.client_locale.as_deref()),
            ("clientVersion", self.client_version.as_deref()),
            ("deviceType", Some(self.device_type.as_str())),
        ] {
            if let Some(value) = value {
                crate::cloud::field(&mut xml, name, value);
            }
        }
        if let Some(product) = &self.product_name {
            crate::cloud::field(&mut xml, "productName", product);
        } else if let Some(name) = &self.device_name {
            crate::cloud::field(&mut xml, "deviceName", name);
        }
        crate::cloud::field(&mut xml, "fingerprint", &self.fingerprint);
        for token in &self.activations {
            xml.push_str("<activationToken>");
            crate::cloud::field(&mut xml, "user", &token.user);
            crate::cloud::field(&mut xml, "device", &token.device);
            if let Some(until) = &token.until {
                crate::cloud::field(&mut xml, "until", until);
            }
            if let (Some(auth), Some(distributor)) = (&token.auth, &token.auth_distributor) {
                crate::cloud::field(&mut xml, "auth", auth);
                crate::cloud::field(&mut xml, "authdistributor", distributor);
            }
            xml.push_str("</activationToken>");
        }
        xml.push_str(&format!("</{tag}>"));
        xml
    }
}
pub const MAX_ACSM_BYTES: usize = 4 * 1024 * 1024;

/// A URL that is not absolute http(s) with a host, without userinfo or fragment.
#[derive(Debug)]
pub struct BadUrl;

/// Accept http or https URLs with a host, no username/password and no fragment.
///
/// Real ACSM files use plain `http://` operator URLs: ADEPT security rests on
/// RSA request signatures, not TLS, so http is accepted.
pub fn require_web_url(url: &str) -> Result<reqwest::Url, BadUrl> {
    let url = reqwest::Url::parse(url.trim()).map_err(|_| BadUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(BadUrl);
    }
    Ok(url)
}

#[derive(Clone, Debug)]
pub struct Acsm {
    /// Keep the entire document: inherited namespace declarations can matter
    /// when importing the token into the signed request DOM.
    xml: String,
    token_range: std::ops::Range<usize>,
    token_namespaces: Vec<(Option<String>, String)>,
    operator_url: String,
}

#[derive(Debug)]
pub enum AcsmError {
    TooLarge,
    Xml(roxmltree::Error),
    MissingToken,
    MissingOperatorUrl,
    BadOperatorUrl,
}

impl fmt::Display for AcsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "ACSM exceeds the local size limit"),
            Self::Xml(error) => write!(f, "invalid ACSM XML: {error}"),
            Self::MissingToken => write!(f, "E_ADEPT_NO_TOKEN"),
            Self::MissingOperatorUrl => write!(f, "E_ADEPT_NO_OPERATOR_URL"),
            Self::BadOperatorUrl => write!(f, "E_ADEPT_BAD_OPERATOR_URL"),
        }
    }
}

impl Error for AcsmError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Xml(error) => Some(error),
            _ => None,
        }
    }
}

impl Acsm {
    pub fn parse(xml: String) -> Result<Self, AcsmError> {
        if xml.len() > MAX_ACSM_BYTES {
            return Err(AcsmError::TooLarge);
        }
        let document = roxmltree::Document::parse_with_options(
            &xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 50_000,
                entity_resolver: None,
            },
        )
        .map_err(AcsmError::Xml)?;
        let token = document
            .descendants()
            .find(|node| node.has_tag_name((ADEPT, "fulfillmentToken")))
            .ok_or(AcsmError::MissingToken)?;
        let operator = token
            .children()
            .find(|node| node.has_tag_name((ADEPT, "operatorURL")))
            .ok_or(AcsmError::MissingOperatorUrl)?;
        let operator_url: String = operator
            .descendants()
            .filter(|node| node.is_text())
            .filter_map(|node| node.text())
            .collect();
        if operator_url.trim().is_empty() {
            return Err(AcsmError::MissingOperatorUrl);
        }
        require_web_url(&operator_url).map_err(|_| AcsmError::BadOperatorUrl)?;
        let token_range = token.range();
        let token_namespaces = token
            .namespaces()
            .map(|namespace| {
                (
                    namespace.name().map(str::to_owned),
                    namespace.uri().to_owned(),
                )
            })
            .collect();
        Ok(Self {
            xml,
            token_range,
            token_namespaces,
            operator_url: operator_url.trim().to_owned(),
        })
    }

    pub fn document_xml(&self) -> &str {
        &self.xml
    }

    /// Original token slice, not necessarily a standalone XML document.
    pub fn token_xml(&self) -> &str {
        &self.xml[self.token_range.clone()]
    }

    pub fn operator_url(&self) -> &str {
        &self.operator_url
    }

    /// Build the fulfill envelope using the selected activation identity.
    /// The token stays byte-for-byte intact; inherited namespace bindings are
    /// carried onto the envelope so nested signatures keep the same infoset.
    pub fn signed_request(
        &self,
        user: &str,
        device: &str,
        device_type: &str,
        descriptors: &[DeviceDescriptor],
        key: &rsa::RsaPrivateKey,
    ) -> Result<String, crate::signing::RequestSigningError> {
        let mut prefix = String::from("request");
        while self
            .token_namespaces
            .iter()
            .any(|(name, _)| name.as_deref() == Some(prefix.as_str()))
        {
            prefix.push('_');
        }
        let mut xml = format!("<{prefix}:fulfill xmlns:{prefix}=\"{ADEPT}\"");
        for (name, uri) in &self.token_namespaces {
            if name.as_deref() == Some("xml") {
                continue;
            }
            xml.push_str(" xmlns");
            if let Some(name) = name {
                xml.push(':');
                xml.push_str(name);
            }
            xml.push_str("=\"");
            for character in uri.chars() {
                match character {
                    '&' => xml.push_str("&amp;"),
                    '<' => xml.push_str("&lt;"),
                    '"' => xml.push_str("&quot;"),
                    '\r' => xml.push_str("&#13;"),
                    '\n' => xml.push_str("&#10;"),
                    '\t' => xml.push_str("&#9;"),
                    other => xml.push(other),
                }
            }
            xml.push('"');
        }
        xml.push('>');
        for (name, value) in [
            ("user", user),
            ("device", device),
            ("deviceType", device_type),
        ] {
            crate::cloud::field(&mut xml, &format!("{prefix}:{name}"), value);
        }
        xml.push_str(self.token_xml());
        for descriptor in descriptors {
            xml.push_str(&descriptor.to_xml());
        }
        xml.push_str(&format!("</{prefix}:fulfill>"));
        crate::signing::sign_request(&xml, key)
    }
}

/// Operation appended to the ACSM operatorURL.
pub const FULFILL_OPERATION: &str = "Fulfill";

/// URL to POST the fulfill request to: operatorURL joined with Fulfill.
pub fn fulfill_url(operator_url: &str) -> String {
    format!(
        "{}/{}",
        operator_url.trim_end_matches('/'),
        FULFILL_OPERATION
    )
}

/// Maximum fulfilled resource (book) size accepted from a download href.
pub const MAX_BOOK_BYTES: usize = 512 * 1024 * 1024;

/// Parsed fulfillment response (fulfillmentResult, resourceItemInfo, resource,
/// src/href, licenseToken, downloadType).
#[derive(Clone, Debug)]
pub struct Fulfillment {
    /// Text of the ADEPT `fulfillment` element.
    pub fulfillment_id: Option<String>,
    /// `dc:title` of the fulfilled item.
    pub title: Option<String>,
    /// `dc:creator` of the fulfilled item.
    pub author: Option<String>,
    /// Smallest `until` under the licenseToken permissions, compared as strings.
    pub loan_expires: Option<String>,
    /// Resource identifier of the fulfilled item.
    pub resource: Option<String>,
    /// URL to download the protected publication from.
    pub download_url: String,
    /// Declared content type of the download, when present (e.g. application/epub+zip).
    pub download_type: Option<String>,
    /// The licenseToken subtree, retained verbatim for later rights handling;
    /// inherited namespace bindings are re-declared so it parses standalone.
    pub license_token_xml: Option<String>,
    /// The loanToken subtree, standalone like `license_token_xml`.
    pub loan_token_xml: Option<String>,
}

/// Original element slice with every inherited in-scope namespace re-declared on its
/// start tag, so the slice parses as a standalone document with the same infoset.
pub fn standalone_element(xml: &str, node: roxmltree::Node) -> String {
    let slice = &xml[node.range()];
    let bytes = slice.as_bytes();
    let name_end = slice[1..]
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .map_or(slice.len(), |index| index + 1);
    // Collect the declarations the start tag already carries.
    let mut declared = Vec::new();
    let mut index = name_end;
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] == b'/' || bytes[index] == b'>' {
            break;
        }
        let start = index;
        while index < bytes.len() && bytes[index] != b'=' && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let name = &slice[start..index];
        while index < bytes.len() && bytes[index] != b'"' && bytes[index] != b'\'' {
            index += 1;
        }
        let Some(&quote) = bytes.get(index) else {
            break;
        };
        index += 1;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        index += 1;
        if name == "xmlns" {
            declared.push(None);
        } else if let Some(prefix) = name.strip_prefix("xmlns:") {
            declared.push(Some(prefix));
        }
    }
    let mut out = String::with_capacity(slice.len() + 64);
    out.push_str(&slice[..name_end]);
    for namespace in node.namespaces() {
        let name = namespace.name();
        if name == Some("xml") || declared.contains(&name) {
            continue;
        }
        out.push_str(" xmlns");
        if let Some(name) = name {
            out.push(':');
            out.push_str(name);
        }
        out.push_str("=\"");
        for character in namespace.uri().chars() {
            match character {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '"' => out.push_str("&quot;"),
                '\r' => out.push_str("&#13;"),
                '\n' => out.push_str("&#10;"),
                '\t' => out.push_str("&#9;"),
                other => out.push(other),
            }
        }
        out.push('"');
    }
    out.push_str(&slice[name_end..]);
    out
}

#[derive(Debug)]
pub enum FulfillResponseError {
    TooLarge,
    Xml(roxmltree::Error),
    /// ADEPT `error` root; the full element is retained.
    Service {
        xml: String,
    },
    /// A fulfillmentResult with no download location.
    MissingDownload,
}

impl fmt::Display for FulfillResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "fulfillment response exceeds the local size limit"),
            Self::Xml(error) => write!(f, "E_ADEPT_XML_SYNTAX: {error}"),
            Self::Service { xml } => match crate::cloud::adept_error_data(xml) {
                Some(data) => write!(f, "E_ADEPT_FULFILL: {data}"),
                None => write!(f, "E_ADEPT_FULFILL: service returned an ADEPT error"),
            },
            Self::MissingDownload => write!(f, "E_ADEPT_NO_DOWNLOAD: no download URL in response"),
        }
    }
}
impl Error for FulfillResponseError {}

/// Parse a fulfillment response. An ADEPT `error` root is a
/// failure; otherwise the download location (`src`, else `href`) and the licenseToken
/// subtree are extracted. The licenseToken is retained verbatim, not reinterpreted.
pub fn parse_fulfillment(xml: &str) -> Result<Fulfillment, FulfillResponseError> {
    if xml.len() > MAX_ACSM_BYTES {
        return Err(FulfillResponseError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(FulfillResponseError::Xml)?;
    let root = document.root_element();
    if root.has_tag_name((ADEPT, "error")) {
        return Err(FulfillResponseError::Service {
            xml: xml.to_owned(),
        });
    }
    let text = |node: roxmltree::Node| -> String {
        node.descendants()
            .filter(|node| node.is_text())
            .filter_map(|node| node.text())
            .collect::<String>()
            .trim()
            .to_owned()
    };
    let find = |name: &str| {
        document
            .descendants()
            .find(|node| node.has_tag_name((ADEPT, name)))
    };
    let download_url = find("src")
        .or_else(|| find("href"))
        .map(text)
        .filter(|value| !value.is_empty())
        .ok_or(FulfillResponseError::MissingDownload)?;
    let download_type = find("downloadType").map(text).filter(|v| !v.is_empty());
    let resource = find("resource").map(text).filter(|v| !v.is_empty());
    let license_token_xml = find("licenseToken").map(|node| standalone_element(xml, node));
    let loan_token_xml = find("loanToken").map(|node| standalone_element(xml, node));
    let dc = |name: &str| {
        document
            .descendants()
            .find(|node| node.has_tag_name((DC, name)))
            .map(text)
            .filter(|v| !v.is_empty())
    };
    let loan_expires = find("licenseToken").and_then(|token| {
        token
            .descendants()
            .find(|node| node.has_tag_name((ADEPT, "permissions")))
            .into_iter()
            .flat_map(|permissions| permissions.descendants())
            .filter(|node| node.has_tag_name((ADEPT, "until")))
            .map(text)
            .filter(|v| !v.is_empty())
            .min()
    });
    Ok(Fulfillment {
        fulfillment_id: find("fulfillment").map(text).filter(|v| !v.is_empty()),
        title: dc("title"),
        author: dc("creator"),
        loan_expires,
        resource,
        download_url,
        download_type,
        license_token_xml,
        loan_token_xml,
    })
}

#[derive(Debug)]
pub enum FulfillError {
    Authorization(String),
    Build(crate::signing::RequestSigningError),
    Transport(crate::service::ServiceError),
    Http {
        status: reqwest::StatusCode,
        body: Vec<u8>,
    },
    Utf8(std::string::FromUtf8Error),
    Response(FulfillResponseError),
    Download(crate::service::ServiceError),
    DownloadHttp {
        status: reqwest::StatusCode,
    },
    BadDownloadUrl,
    BadOperatorUrl,
}

impl fmt::Display for FulfillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authorization(error) => write!(f, "provider authorization failed: {error}"),
            Self::Build(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Http { status, .. } => write!(f, "fulfillment HTTP request failed: {status}"),
            Self::Utf8(error) => write!(f, "fulfillment response is not UTF-8: {error}"),
            Self::Response(error) => error.fmt(f),
            Self::Download(error) => write!(f, "book download failed: {error}"),
            Self::DownloadHttp { status } => write!(f, "book download failed: HTTP {status}"),
            Self::BadDownloadUrl => write!(f, "E_ADEPT_BAD_DOWNLOAD_URL"),
            Self::BadOperatorUrl => write!(f, "E_ADEPT_BAD_OPERATOR_URL"),
        }
    }
}
impl Error for FulfillError {}

/// Initialize an authenticated operator before its first fulfillment on this device:
/// send the credential certificates to `/Auth`, then register the operator with the
/// activation service via `/InitLicenseService`.
/// The caller must persist changed activation state, even if the later download fails.
pub fn prepare_operator(
    client: &dyn crate::service::Transport,
    state: &mut crate::store::ActivationState,
    acsm: &Acsm,
) -> Result<bool, FulfillError> {
    let operator = acsm.operator_url().trim_end_matches('/');
    let document = roxmltree::Document::parse(acsm.document_xml())
        .map_err(|e| FulfillError::Authorization(e.to_string()))?;
    let anonymous = document
        .descendants()
        .find(|n| n.has_tag_name((ADEPT, "fulfillmentToken")))
        .is_some_and(|n| n.attribute("auth") == Some("none"));
    if anonymous {
        return Ok(false);
    }
    let token = state
        .activation_token()
        .ok_or_else(|| FulfillError::Authorization("device has not been activated".into()))?;
    if token.user != state.user
        || token.device != state.device
        || token.fingerprint != state.fingerprint
    {
        return Err(FulfillError::Authorization(
            "stored device activation does not match this account".into(),
        ));
    }
    if state
        .operator_urls
        .iter()
        .any(|url| url.trim_end_matches('/') == operator)
    {
        return Ok(false);
    }
    let request = operator_auth_request(state)?;
    authorize_exchange(client, &format!("{operator}/Auth"), request, "Auth")?;

    // The activation service, rather than the operator, signs this account up for
    // future license issuance. It has its own endpoint returned by Activate.
    require_web_url(&token.activation_url)
        .map_err(|_| FulfillError::Authorization("invalid activation service URL".into()))?;
    let mut request = format!(r#"<licenseServiceRequest xmlns="{ADEPT}" identity="user">"#);
    crate::cloud::field(&mut request, "operatorURL", operator);
    crate::cloud::field(&mut request, "nonce", &crate::signing::nonce());
    crate::cloud::field(&mut request, "expiration", &crate::signing::expiration());
    crate::cloud::field(&mut request, "user", &state.user);
    request.push_str("</licenseServiceRequest>");
    let request = crate::signing::sign_request(&request, &state.keys.authentication)
        .map_err(FulfillError::Build)?;
    authorize_exchange(
        client,
        &format!(
            "{}/InitLicenseService",
            token.activation_url.trim_end_matches('/')
        ),
        request,
        "InitLicenseService",
    )?;
    state.operator_urls.push(operator.to_owned());
    Ok(true)
}

fn authorize_exchange(
    client: &dyn crate::service::Transport,
    url: &str,
    request: String,
    operation: &str,
) -> Result<(), FulfillError> {
    let response = client
        .post_adept(url, request)
        .map_err(|e| FulfillError::Authorization(format!("{operation}: {e}")))?;
    let xml = std::str::from_utf8(&response.body)
        .map_err(|_| FulfillError::Authorization(format!("{operation}: response is not UTF-8")))?;
    check_not_error(xml).map_err(|e| FulfillError::Authorization(format!("{operation}: {e}")))?;
    if !response.status.is_success() {
        return Err(FulfillError::Authorization(format!(
            "{operation}: HTTP {}",
            response.status
        )));
    }
    Ok(())
}

fn operator_auth_request(state: &crate::store::ActivationState) -> Result<String, FulfillError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rsa::pkcs8::DecodePublicKey;
    use x509_cert::{
        Certificate,
        der::{Decode, Encode},
    };
    use zeroize::Zeroizing;

    let missing = || {
        FulfillError::Authorization(
            "stored credentials lack the authentication certificates; sign in again".into(),
        )
    };
    let document = roxmltree::Document::parse_with_options(
        &state.credentials_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(|_| missing())?;
    let text = |name| -> Option<String> {
        let node = document
            .descendants()
            .find(|n| n.has_tag_name((ADEPT, name)))?;
        let text: String = node
            .descendants()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .flat_map(str::chars)
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        (!text.is_empty()).then_some(text)
    };
    let license = text("licenseCertificate").ok_or_else(missing)?;
    let authentication = text("authenticationCertificate").ok_or_else(missing)?;
    let pfx = Zeroizing::new(
        STANDARD
            .decode(text("pkcs12").ok_or_else(missing)?)
            .map_err(|_| missing())?,
    );
    let pfx = p12::PFX::parse(&pfx).map_err(|_| missing())?;
    let password = Zeroizing::new(
        STANDARD
            .encode(state.device_key)
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<u8>>(),
    );
    let safe = Zeroizing::new(pfx.auth_safe.data(&password).ok_or_else(missing)?);
    // Adobe's PKCS#12 uses BER; p12's high-level certificate walker requires DER.
    let contents = yasna::parse_ber(&safe, |r| r.collect_sequence_of(p12::ContentInfo::parse))
        .map_err(|_| missing())?;
    let public_key = state.keys.authentication.to_public_key();
    let mut certificate = None;
    for content in &contents {
        let Some(data) = content.data(&password) else {
            continue;
        };
        let data = Zeroizing::new(data);
        let Ok(bags) = yasna::parse_ber(&data, |r| r.collect_sequence_of(p12::SafeBag::parse))
        else {
            continue;
        };
        certificate = bags
            .iter()
            .filter_map(|bag| bag.bag.get_x509_cert())
            .find(|der| {
                let Ok(cert) = Certificate::from_der(der) else {
                    return false;
                };
                let Ok(spki) = cert.tbs_certificate().subject_public_key_info().to_der() else {
                    return false;
                };
                rsa::RsaPublicKey::from_public_key_der(&spki).is_ok_and(|key| key == public_key)
            });
        if certificate.is_some() {
            break;
        }
    }
    let certificate = certificate.ok_or_else(missing)?;
    let mut xml = format!(r#"<credentials xmlns="{ADEPT}">"#);
    crate::cloud::field(&mut xml, "user", &state.user);
    crate::cloud::field(&mut xml, "certificate", &STANDARD.encode(certificate));
    crate::cloud::field(&mut xml, "licenseCertificate", &license);
    crate::cloud::field(&mut xml, "authenticationCertificate", &authentication);
    xml.push_str("</credentials>");
    Ok(xml)
}

/// The fulfilled publication: the parsed fulfillment plus the downloaded book bytes.
pub struct FulfilledBook {
    pub fulfillment: Fulfillment,
    pub body: Vec<u8>,
}

/// Run one fulfillment: POST the signed fulfill request to operatorURL/Fulfill, parse
/// the response, and download the publication. The caller owns the device identity and
/// signing key (from activation state) and persists the result. A single-use token is
/// not retried on transport failure.
#[allow(clippy::too_many_arguments)]
pub fn perform_fulfillment(
    client: &dyn crate::service::Transport,
    acsm: &Acsm,
    user: &str,
    device: &str,
    device_type: &str,
    descriptors: &[DeviceDescriptor],
    key: &rsa::RsaPrivateKey,
) -> Result<FulfilledBook, FulfillError> {
    let request = acsm
        .signed_request(user, device, device_type, descriptors, key)
        .map_err(FulfillError::Build)?;
    let response = client
        .post_adept(&fulfill_url(acsm.operator_url()), request)
        .map_err(FulfillError::Transport)?;
    if !response.status.is_success() {
        if let Ok(xml) = std::str::from_utf8(&response.body)
            && let Err(error @ FulfillResponseError::Service { .. }) = parse_fulfillment(xml)
        {
            return Err(FulfillError::Response(error));
        }
        return Err(FulfillError::Http {
            status: response.status,
            body: response.body,
        });
    }
    let xml = String::from_utf8(response.body).map_err(FulfillError::Utf8)?;
    let fulfillment = parse_fulfillment(&xml).map_err(FulfillError::Response)?;
    let download_url =
        require_web_url(&fulfillment.download_url).map_err(|_| FulfillError::BadDownloadUrl)?;
    let download = client
        .fetch(download_url.as_str(), MAX_BOOK_BYTES)
        .map_err(FulfillError::Download)?;
    if !download.status.is_success() {
        return Err(FulfillError::DownloadHttp {
            status: download.status,
        });
    }
    Ok(FulfilledBook {
        fulfillment,
        body: download.body,
    })
}

/// Operation appended to the operatorURL for a loan return.
pub const LOAN_RETURN_OPERATION: &str = "LoanReturn";

/// URL to POST a loan return to: operatorURL joined with LoanReturn.
pub fn loan_return_url(operator_url: &str) -> String {
    format!(
        "{}/{}",
        operator_url.trim_end_matches('/'),
        LOAN_RETURN_OPERATION
    )
}

/// Build a signed loanReturn request. Field order (user, device, loanToken) follows the
/// other ADEPT requests; the loanToken is the subtree issued with the loan, carried
/// back verbatim.
pub fn loan_return_request(
    user: &str,
    device: &str,
    loan_token_xml: &str,
    key: &rsa::RsaPrivateKey,
) -> Result<String, crate::signing::RequestSigningError> {
    let mut xml = format!("<loanReturn xmlns=\"{ADEPT}\">");
    crate::cloud::field(&mut xml, "user", user);
    crate::cloud::field(&mut xml, "device", device);
    xml.push_str(loan_token_xml);
    xml.push_str("</loanReturn>");
    crate::signing::sign_request(&xml, key)
}

/// Fail when a response body is an ADEPT `error` root; any other well-formed XML passes.
fn check_not_error(xml: &str) -> Result<(), FulfillResponseError> {
    if xml.len() > MAX_ACSM_BYTES {
        return Err(FulfillResponseError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(FulfillResponseError::Xml)?;
    if document.root_element().has_tag_name((ADEPT, "error")) {
        return Err(FulfillResponseError::Service {
            xml: xml.to_owned(),
        });
    }
    Ok(())
}

/// Perform a loan return: POST the signed request to operatorURL/LoanReturn. A well-formed
/// non-error 2xx response is acceptance; an ADEPT `error` root (even with 2xx) is
/// retained and returned.
pub fn perform_loan_return(
    client: &dyn crate::service::Transport,
    operator_url: &str,
    user: &str,
    device: &str,
    loan_token_xml: &str,
    key: &rsa::RsaPrivateKey,
) -> Result<(), FulfillError> {
    require_web_url(operator_url).map_err(|_| FulfillError::BadOperatorUrl)?;
    let request =
        loan_return_request(user, device, loan_token_xml, key).map_err(FulfillError::Build)?;
    let response = client
        .post_adept(&loan_return_url(operator_url), request)
        .map_err(FulfillError::Transport)?;
    if !response.status.is_success() {
        if let Ok(xml) = std::str::from_utf8(&response.body)
            && let Err(error @ FulfillResponseError::Service { .. }) = check_not_error(xml)
        {
            return Err(FulfillError::Response(error));
        }
        return Err(FulfillError::Http {
            status: response.status,
            body: response.body,
        });
    }
    let xml = String::from_utf8(response.body).map_err(FulfillError::Utf8)?;
    check_not_error(&xml).map_err(FulfillError::Response)
}
