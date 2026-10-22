//! Concrete PK13 transport over tauri-plugin-updater's signature verifier & installer.
//! Construct after installing the app's existing updater plugin. Endpoints, signing key &
//! timeout are native configuration; no updater plugin IPC permission is needed by a renderer.
//! All redirects fail closed, including redirects between allowed hosts. Headers are authorized
//! separately for check & download, then removed from the cached native update object.

use std::cell::{Cell, RefCell};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Url, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

use super::{
    current_platform, lock, ReleaseInfo, RequestAuthorizer, RequestHeaders, StagedArtifact,
    StagedIdentity, TransportError, UpdateKind, UpdateTransport,
};

/// Native application configuration. `endpoint` is the existing Tauri updater feed, including
/// Tauri's optional version/target/architecture placeholders. `public_key` is the app's pinned
/// updater verification key. Never obtain either value from IPC.
pub struct TauriUpdateConfig {
    pub endpoint: Url,
    pub public_key: String,
    pub timeout: Duration,
}

/// Shared updater implementation; apps configure it instead of implementing UpdateTransport.
pub struct TauriUpdateTransport {
    app: AppHandle<Wry>,
    config: TauriUpdateConfig,
    // One check generation only. UpdateHost invalidates older handles on its next check.
    checked: Mutex<Option<(ReleaseInfo, Update)>>,
}

struct VerifiedBytes {
    update: Update,
    bytes: Vec<u8>,
}

impl TauriUpdateTransport {
    pub fn new(app: AppHandle<Wry>, config: TauriUpdateConfig) -> Result<Self, TransportError> {
        if config.timeout.is_zero() || config.public_key.trim().is_empty() {
            return Err(TransportError::Failed);
        }
        Ok(Self {
            app,
            config,
            checked: Mutex::new(None),
        })
    }
}

fn request_headers(headers: &RequestHeaders) -> Result<HeaderMap, TransportError> {
    let mut result = HeaderMap::new();
    let mut invalid = false;
    headers.each(|name, value| {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(mut value)) => {
                // Prevent HTTP debug output from exposing credentials.
                value.set_sensitive(true);
                result.insert(name, value);
            }
            _ => invalid = true,
        }
    });
    if invalid {
        Err(TransportError::Failed)
    } else {
        Ok(result)
    }
}

fn client_policy(client: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    client
        .https_only(true)
        .tls_danger_accept_invalid_certs(false)
        .tls_danger_accept_invalid_hostnames(false)
        .referer(false)
        .redirect(redirect_policy())
}

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::none()
}

fn identity(bytes: &[u8]) -> StagedIdentity {
    StagedIdentity {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        length: bytes.len() as u64,
    }
}

fn map_plugin_error(error: tauri_plugin_updater::Error) -> TransportError {
    use tauri_plugin_updater::Error;
    match error {
        Error::Reqwest(_) | Error::Network(_) | Error::ReleaseNotFound => TransportError::Network,
        Error::Minisign(_) | Error::Base64(_) | Error::SignatureUtf8(_) => TransportError::Verify,
        _ => TransportError::Failed,
    }
}

fn release_info(update: &Update) -> Result<ReleaseInfo, TransportError> {
    // Existing @rightkit/app-update manifest contract: src/index.ts readTier().
    // An absent/unknown tier must never silently become a public patch.
    let (kind, tier) = match update.raw_json.get("tier").and_then(|v| v.as_str()) {
        Some("patch") => (UpdateKind::Patch, "patch"),
        Some("update") => (UpdateKind::Update, "update"),
        _ => return Err(TransportError::Verify),
    };
    Ok(ReleaseInfo {
        release_id: update.version.clone(),
        version: update.version.clone(),
        kind,
        date: update.date.map(|date| date.to_string()),
        // Raw feed text may contain URLs or sensitive metadata; it never crosses this adapter.
        notes_summary: None,
        tier: Some(tier.into()),
        platform: current_platform(),
        artifact_url: update.download_url.clone(),
    })
}

fn same_release(left: &ReleaseInfo, right: &ReleaseInfo) -> bool {
    left.release_id == right.release_id
        && left.version == right.version
        && left.kind == right.kind
        && left.tier == right.tier
        && left.platform == right.platform
        && left.artifact_url == right.artifact_url
}

impl UpdateTransport for TauriUpdateTransport {
    fn check(&self, auth: &RequestAuthorizer<'_>) -> Result<Option<ReleaseInfo>, TransportError> {
        // Retire any previous native installer object even if authorization now refuses.
        *lock(&self.checked) = None;
        let headers = auth
            .authorize(&self.config.endpoint)
            .map_err(TransportError::Refused)?;
        let updater = self
            .app
            .updater_builder()
            .clear_headers()
            .headers(request_headers(&headers)?)
            .endpoints(vec![self.config.endpoint.clone()])
            .map_err(map_plugin_error)?
            .pubkey(&self.config.public_key)
            .timeout(self.config.timeout)
            .configure_client(client_policy)
            .version_comparator(|current, release| release.version > current)
            .build()
            .map_err(map_plugin_error)?;
        let found = tauri::async_runtime::block_on(updater.check()).map_err(map_plugin_error)?;
        let Some(mut update) = found else {
            return Ok(None);
        };
        // Update::check copies check headers into Update. They must not become download headers.
        update.headers.clear();
        update.timeout = Some(self.config.timeout);
        let release = release_info(&update)?;
        // Verify artifact host eligibility before retaining its installer object.
        // Header authorization is repeated with release-bound credentials when downloading.
        if !auth.hosts.allows(&release.artifact_url) {
            return Err(TransportError::Refused(super::UpdateRefusal::NotLicensed));
        }
        *lock(&self.checked) = Some((release.clone(), update));
        Ok(Some(release))
    }

    fn download(
        &self,
        release: &ReleaseInfo,
        auth: &RequestAuthorizer<'_>,
        progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<StagedArtifact, TransportError> {
        let mut update = {
            let checked = lock(&self.checked);
            let (found, update) = checked.as_ref().ok_or(TransportError::Failed)?;
            if !same_release(found, release) {
                return Err(TransportError::Verify);
            }
            update.clone()
        };
        let headers = auth
            .authorize(&release.artifact_url)
            .map_err(TransportError::Refused)?;
        update.headers = request_headers(&headers)?;
        let bytes = tauri::async_runtime::block_on(download_verified(
            &update,
            self.config.timeout,
            progress,
        ))?;
        update.headers.clear();
        let staged_identity = identity(&bytes);
        Ok(StagedArtifact {
            identity: staged_identity,
            token: Box::new(VerifiedBytes { update, bytes }),
        })
    }

    fn staged_identity(&self, staged: &StagedArtifact) -> Result<StagedIdentity, TransportError> {
        let staged = staged
            .token
            .downcast_ref::<VerifiedBytes>()
            .ok_or(TransportError::Verify)?;
        Ok(identity(&staged.bytes))
    }

    fn install(&self, staged: StagedArtifact) -> Result<(), TransportError> {
        // UpdateHost already checks the original licence revision & recorded digest immediately
        // before this call. Recheck identity here too; public callers cannot substitute a token.
        let expected = staged.identity;
        let staged = staged
            .token
            .downcast::<VerifiedBytes>()
            .map_err(|_| TransportError::Verify)?;
        if identity(&staged.bytes) != expected {
            return Err(TransportError::Verify);
        }
        staged
            .update
            .install(&staged.bytes)
            .map_err(|_| TransportError::Interrupted)
    }
}

async fn download_verified(
    update: &Update,
    timeout: Duration,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<Vec<u8>, TransportError> {
    let received = Cell::new(0u64);
    let total = Cell::new(None);
    let cancelled = Cell::new(false);
    let progress = RefCell::new(progress);
    if !(progress.borrow_mut())(0, None) {
        return Err(TransportError::Cancelled);
    }
    // Official plugin download performs minisign verification before returning bytes.
    let download = update.download(
        |chunk, expected| {
            received.set(received.get().saturating_add(chunk as u64));
            total.set(expected);
            if !(progress.borrow_mut())(received.get(), expected) {
                cancelled.set(true);
            }
        },
        || {},
    );
    tokio::pin!(download);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        // Dropping download cancels reqwest's request/body future; no detached download task.
        if cancelled.get() {
            return Err(TransportError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = &mut deadline => return Err(TransportError::Network),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if !(progress.borrow_mut())(received.get(), total.get()) {
                    return Err(TransportError::Cancelled);
                }
            }
            result = &mut download => {
                if cancelled.get() { return Err(TransportError::Cancelled) }
                return result.map_err(map_plugin_error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_never_reaches_second_host_or_forwards_authorization() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let source_address = source.local_addr().unwrap();
        let target_address = target.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = source.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0u8; 4096];
            // The mock answers any request with a redirect: whatever part of the request line arrived is enough.
            let _request_len = stream.read(&mut request).unwrap();
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        // reqwest is built with rustls-no-provider; tauri-plugin-updater installs the ring provider before it
        // builds its client, so do the same here or Client::build panics.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        // HTTP is limited to this in-process fixture. Production client_policy requires HTTPS.
        let result = tauri::async_runtime::block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(redirect_policy())
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap()
                .get(format!("http://{source_address}/check"))
                .header("Authorization", "Bearer fixture-only")
                .send()
                .await
        });
        server.join().unwrap();
        assert_eq!(result.unwrap().status(), reqwest::StatusCode::FOUND);
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn invalid_header_cannot_reach_http_and_debug_is_redacted() {
        assert!(request_headers(&RequestHeaders::new(vec![(
            "authorization".into(),
            "bad\r\nheader".into()
        )]))
        .is_err());
        let headers = request_headers(&RequestHeaders::new(vec![(
            "authorization".into(),
            "Bearer hidden-secret".into(),
        )]))
        .unwrap();
        assert!(headers["authorization"].is_sensitive());
        assert!(!format!("{headers:?}").contains("hidden-secret"));
    }

    #[test]
    fn altered_bytes_fail_identity_before_install() {
        let before = identity(b"signed bytes");
        let after = identity(b"swapped bytes");
        assert_ne!(before, after);
        assert_eq!(before.sha256.len(), 64);
        assert_ne!(before, identity(b"signed bytes\0"));
    }

    #[test]
    fn forged_release_cannot_select_checked_installer() {
        let release = ReleaseInfo {
            release_id: "1.2.3".into(),
            version: "1.2.3".into(),
            kind: UpdateKind::Update,
            date: None,
            notes_summary: None,
            tier: Some("update".into()),
            platform: current_platform(),
            artifact_url: Url::parse("https://allowed.example/build").unwrap(),
        };
        let mut forged = release.clone();
        forged.artifact_url = Url::parse("https://evil.example/build").unwrap();
        assert!(!same_release(&release, &forged));
        forged = release.clone();
        forged.kind = UpdateKind::Patch;
        assert!(!same_release(&release, &forged));
    }
}
