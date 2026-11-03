use super::{DirectAuthKind, DirectConnection};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectValidationError {
    InvalidConnection,
    InvalidUrl,
    TlsRequired,
    PrivateHostApprovalRequired,
    ForbiddenHost,
    InvalidHeader,
}

impl DirectValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConnection => "invalid_connection",
            Self::InvalidUrl => "invalid_url",
            Self::TlsRequired => "tls_required",
            Self::PrivateHostApprovalRequired => "private_host_approval_required",
            Self::ForbiddenHost => "forbidden_host",
            Self::InvalidHeader => "invalid_header",
        }
    }
}

pub fn normalize_base_url(raw: &str) -> Result<String, DirectValidationError> {
    let mut raw = raw.trim().trim_end_matches('/');
    while raw.ends_with("/v1/v1") {
        raw = raw.strip_suffix("/v1").unwrap_or(raw);
    }
    let url = url::Url::parse(raw).map_err(|_| DirectValidationError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(DirectValidationError::InvalidUrl);
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// Check every address returned for a host.  This is deliberately separate
/// from connection validation: DNS may rebind after configuration is saved.
pub fn validate_resolved_addresses(
    addresses: impl IntoIterator<Item = IpAddr>,
    trust_private_network: bool,
) -> Result<(), DirectValidationError> {
    use rightkit_http::netpolicy::{classify_ip, AddressClass};
    let mut any = false;
    for ip in addresses {
        any = true;
        // Address classes come from rightkit-http's outbound address policy.
        match classify_ip(ip) {
            AddressClass::Public | AddressClass::Loopback => {}
            AddressClass::Private if trust_private_network => {}
            AddressClass::Private => {
                return Err(DirectValidationError::PrivateHostApprovalRequired)
            }
            _ => return Err(DirectValidationError::ForbiddenHost),
        }
    }
    any.then_some(())
        .ok_or(DirectValidationError::ForbiddenHost)
}

/// Resolve and validate immediately before a Direct request.  Rechecking all
/// returned addresses prevents a hostname with a mixed or rebound answer from
/// becoming a credential-bearing SSRF hop.
pub async fn validate_direct_request_target(
    connection: &DirectConnection,
) -> Result<(), DirectValidationError> {
    resolved_direct_addresses(connection).await.map(|_| ())
}

/// Return approved addresses for a request. Callers must pin these on their
/// HTTP client instead of asking DNS a second time after validation.
pub async fn resolved_direct_addresses(
    connection: &DirectConnection,
) -> Result<Vec<SocketAddr>, DirectValidationError> {
    validate_direct_connection(connection)?;
    let url = url::Url::parse(&connection.normalized_base_url)
        .map_err(|_| DirectValidationError::InvalidUrl)?;
    let host = url.host_str().ok_or(DirectValidationError::InvalidUrl)?;
    let port = url
        .port_or_known_default()
        .ok_or(DirectValidationError::InvalidUrl)?;
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        validate_resolved_addresses([ip], connection.trust_private_network)?;
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    // Resolve once and vet every answer with rightkit-http's address policy: loopback
    // is always allowed, a private network only when the connection trusts it.
    let trust_private = connection.trust_private_network;
    let host = host.to_string();
    let policy = rightkit_http::AddressPolicy::public_web()
        .allow_if(vec![host.clone()], move |ip| {
            ip.is_loopback() || trust_private
        });
    tokio::task::spawn_blocking(move || policy.resolve_vetted(&host, port))
        .await
        .map_err(|_| DirectValidationError::ForbiddenHost)?
        .map_err(|error| match error {
            rightkit_http::HttpError::AddressDenied {
                class: Some(rightkit_http::AddressClass::Private),
                ..
            } => DirectValidationError::PrivateHostApprovalRequired,
            _ => DirectValidationError::ForbiddenHost,
        })
}
fn denied_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
    )
}
fn valid_token(v: &str) -> bool {
    !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

pub fn validate_direct_connection(
    connection: &DirectConnection,
) -> Result<(), DirectValidationError> {
    if connection.name.trim().is_empty()
        || connection.name.chars().count() > 80
        || connection.manual_model_ids.len() > 500
        || connection.public_headers.len() > 32
        || connection
            .manual_model_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
        || connection
            .manual_model_ids
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != connection.manual_model_ids.len()
    {
        return Err(DirectValidationError::InvalidConnection);
    }
    let normalized = normalize_base_url(&connection.normalized_base_url)?;
    if normalized != connection.normalized_base_url {
        return Err(DirectValidationError::InvalidUrl);
    }
    let url = url::Url::parse(&normalized).map_err(|_| DirectValidationError::InvalidUrl)?;
    if url.scheme() == "http" && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    {
        return Err(DirectValidationError::TlsRequired);
    }
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<IpAddr>() {
            validate_resolved_addresses([ip], connection.trust_private_network)?;
        }
    }
    if connection.public_headers.iter().any(|(k, v)| {
        k.is_empty() || k.len() > 128 || v.len() > 1024 || denied_header(k) || !valid_token(k)
    }) {
        return Err(DirectValidationError::InvalidHeader);
    }
    match connection.auth_kind {
        DirectAuthKind::None if connection.auth_header_name.is_some() => {
            Err(DirectValidationError::InvalidConnection)
        }
        DirectAuthKind::ApiKeyHeader
            if !connection
                .auth_header_name
                .as_deref()
                .is_some_and(|h| valid_token(h) && !denied_header(h)) =>
        {
            Err(DirectValidationError::InvalidHeader)
        }
        _ => Ok(()),
    }
}
