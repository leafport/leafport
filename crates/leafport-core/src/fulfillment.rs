//! ACSM input for the fulfillment request.
//!
//! Transport and signing are not yet implemented here.

use std::{error::Error, fmt};

const ADEPT: &str = "http://ns.adobe.com/adept";

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
}

impl fmt::Display for AcsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "ACSM exceeds the local size limit"),
            Self::Xml(error) => write!(f, "invalid ACSM XML: {error}"),
            Self::MissingToken => write!(f, "E_ADEPT_NO_TOKEN"),
            Self::MissingOperatorUrl => write!(f, "E_ADEPT_NO_OPERATOR_URL"),
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
            operator_url,
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
    /// Resource identifier of the fulfilled item.
    pub resource: Option<String>,
    /// URL to download the protected publication from.
    pub download_url: String,
    /// Declared content type of the download, when present (e.g. application/epub+zip).
    pub download_type: Option<String>,
    /// The licenseToken subtree, retained verbatim for later rights handling.
    pub license_token_xml: Option<String>,
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
            Self::Service { .. } => write!(f, "E_ADEPT_FULFILL: service returned an ADEPT error"),
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
    let license_token_xml = find("licenseToken").map(|node| {
        let range = node.range();
        xml[range].to_owned()
    });
    Ok(Fulfillment {
        resource,
        download_url,
        download_type,
        license_token_xml,
    })
}

#[derive(Debug)]
pub enum FulfillError {
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
}

impl fmt::Display for FulfillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Http { status, .. } => write!(f, "fulfillment HTTP request failed: {status}"),
            Self::Utf8(error) => write!(f, "fulfillment response is not UTF-8: {error}"),
            Self::Response(error) => error.fmt(f),
            Self::Download(error) => write!(f, "book download failed: {error}"),
            Self::DownloadHttp { status } => write!(f, "book download failed: HTTP {status}"),
        }
    }
}
impl Error for FulfillError {}

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
    client: &crate::service::ServiceClient,
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
        if let Ok(xml) = std::str::from_utf8(&response.body) {
            if let Err(error @ FulfillResponseError::Service { .. }) = parse_fulfillment(xml) {
                return Err(FulfillError::Response(error));
            }
        }
        return Err(FulfillError::Http {
            status: response.status,
            body: response.body,
        });
    }
    let xml = String::from_utf8(response.body).map_err(FulfillError::Utf8)?;
    let fulfillment = parse_fulfillment(&xml).map_err(FulfillError::Response)?;
    let download = client
        .fetch(&fulfillment.download_url, MAX_BOOK_BYTES)
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

/// Perform a loan return: POST the signed request to operatorURL/LoanReturn. A non-error
/// response is treated as acceptance; an ADEPT `error` root is retained and returned.
pub fn perform_loan_return(
    client: &crate::service::ServiceClient,
    operator_url: &str,
    user: &str,
    device: &str,
    loan_token_xml: &str,
    key: &rsa::RsaPrivateKey,
) -> Result<(), FulfillError> {
    let request =
        loan_return_request(user, device, loan_token_xml, key).map_err(FulfillError::Build)?;
    let response = client
        .post_adept(&loan_return_url(operator_url), request)
        .map_err(FulfillError::Transport)?;
    if !response.status.is_success() {
        if let Ok(xml) = std::str::from_utf8(&response.body) {
            if let Err(error @ FulfillResponseError::Service { .. }) = parse_fulfillment(xml) {
                return Err(FulfillError::Response(error));
            }
        }
        return Err(FulfillError::Http {
            status: response.status,
            body: response.body,
        });
    }
    Ok(())
}
