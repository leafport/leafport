//! Dynamic sign-in choices: Adobe ID plus the providers the service announces.

use std::{error::Error, fmt};

/// Local resource limit, not a server-side protocol limit.
pub const MAX_SERVICE_INFO_BYTES: usize = 4 * 1024 * 1024;
const MAX_XML_NODES: u32 = 50_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vendor {
    pub method: String,
    pub node_id: Option<String>,
    pub sign_in_type: Option<String>,
    pub name: String,
}

/// Public activation prerequisites returned by AuthenticationServiceInfo.
/// The certificate is retained as encoded by the server; it is not a trust decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthenticationServiceInfo {
    pub auth_url: Option<String>,
    pub certificate: Option<String>,
}

#[derive(Debug)]
pub enum VendorError {
    InputTooLarge,
    InvalidXml(roxmltree::Error),
}

impl fmt::Display for VendorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge => write!(
                f,
                "authentication service information exceeds the size limit"
            ),
            Self::InvalidXml(error) => write!(f, "invalid authentication service XML: {error}"),
        }
    }
}

impl Error for VendorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidXml(error) => Some(error),
            Self::InputTooLarge => None,
        }
    }
}

#[derive(Debug)]
pub struct VendorCatalog {
    entries: Vec<Vendor>,
    loaded: bool,
    service_info: AuthenticationServiceInfo,
}

impl Default for VendorCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl VendorCatalog {
    pub fn new() -> Self {
        Self {
            entries: vec![Vendor {
                method: "AdobeID".into(),
                node_id: None,
                sign_in_type: None,
                name: "Adobe ID".into(),
            }],
            loaded: false,
            service_info: AuthenticationServiceInfo::default(),
        }
    }

    pub fn entries(&self) -> &[Vendor] {
        &self.entries
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn service_info(&self) -> &AuthenticationServiceInfo {
        &self.service_info
    }

    /// Append discovered choices after the built-in ones.
    /// A failed parse leaves both entries and load state untouched.
    pub fn apply_service_info(&mut self, xml: &str) -> Result<(), VendorError> {
        if xml.len() > MAX_SERVICE_INFO_BYTES {
            return Err(VendorError::InputTooLarge);
        }
        let document = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_XML_NODES,
                entity_resolver: None,
            },
        )
        .map_err(VendorError::InvalidXml)?;
        let root = document.root_element();
        let namespace = root.tag_name().namespace();
        let field = |name| {
            root.children()
                .find(|node| {
                    node.is_element()
                        && node.tag_name().namespace() == namespace
                        && node.tag_name().name() == name
                })
                .map(|node| {
                    node.descendants()
                        .filter(|child| child.is_text())
                        .filter_map(|child| child.text())
                        .collect::<String>()
                        .trim()
                        .to_owned()
                })
        };
        let service_info = AuthenticationServiceInfo {
            auth_url: field("authURL"),
            certificate: field("certificate"),
        };
        if let Some(methods) = root.children().find(|node| {
            node.is_element()
                && node.tag_name().namespace() == namespace
                && node.tag_name().name() == "signInMethods"
        }) {
            for node in methods.children().filter(|node| {
                node.is_element()
                    && node.tag_name().namespace() == namespace
                    && node.tag_name().name() == "signInMethod"
            }) {
                let method = node.attribute("method").unwrap_or("");
                if matches!(method, "anonymous" | "AdobeID") {
                    continue;
                }
                let name = node
                    .descendants()
                    .filter(|child| child.is_text())
                    .filter_map(|child| child.text())
                    .collect();
                self.entries.push(Vendor {
                    method: method.to_owned(),
                    node_id: node.attribute("nodeId").map(str::to_owned),
                    sign_in_type: node.attribute("type").map(str::to_owned),
                    name,
                });
            }
        }
        self.loaded = true;
        self.service_info = service_info;
        Ok(())
    }
}
