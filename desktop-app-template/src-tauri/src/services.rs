//! The foundation owners the window host registers once, under one caller policy
//! (`rightkit_tauri_shell::host`, features `host-services` and `license`):
//!
//! * M10 settings: one `rightkit_app_settings` document under the data root, behind the
//!   `rightkit_settings_*` commands that `@rightkit/settings-ui/shell-adapter` and `@rightkit/onboarding` call.
//! * M14 legal acceptance: the native legal receipt (`rightkit_legal_receipt_*`). The renderer declares what the
//!   gate showed; the service compares it with the manifest compiled in here (`legal/legal-manifest.json` and the
//!   generated `src/generated/legal-contract.json`), stamps the time itself and stores the receipt.
//! * M12/M13 licence: `rightkit_license::LicenseClient` behind the `rightkit_license_*` commands
//!   (`@rightkit/license`). Until the app registers its appKey, licence host and public key ([`LICENSE`]), the client
//!   is `LicenseSetup::Disabled`: `status` answers `mode: disabled`, nothing touches the network, and the app runs.
//! * M17 HTTP: [`AppHttp`], the one `rightkit_http` client for the app's own calls, pinned to the registered licence
//!   host (first-party: that host only, HTTPS only). There is none while licensing is disabled.
//!
//! Every service trusts the `main` window only. The renderer never names a path, a namespace or a key.

use std::path::Path;
use std::sync::Arc;

use rightkit_app_settings::Document;
use rightkit_credentials::Store;
use rightkit_device::{credentials::CredentialStore, DeviceService};
use rightkit_license::{
    DeviceIdError, DeviceIdSource, LicenseClient, LicenseClientConfig, LicenseSetup,
};
use rightkit_tauri_shell::host::{
    DeclaredDocument, DocumentReceiptStore, HostServices, LegalManifest, LegalReceiptService,
    ServiceTrust, SettingsBridge, SettingsHostService, StaticSchema,
};
use serde_json::Value;

/// The only window the template has (`tauri.conf.json`); every host service trusts it and nothing else.
pub const TRUSTED_WINDOWS: [&str; 1] = ["main"];

/// The settings document's namespace and schema version. Bump the version with a migration in the renderer.
pub const SETTINGS_NAMESPACE: &str = "rightapp";
pub const SETTINGS_VERSION: u32 = 1;
/// Top-level settings keys the renderer may write. `onboarding.state` is `@rightkit/onboarding`'s progress and its
/// mirror of the legal acceptance. Add each key the app's own settings write.
pub const SETTINGS_KEYS: [&str; 1] = ["onboarding.state"];

/// The legal receipt lives in its own document, beside the settings, so a settings reset never clears it.
const LEGAL_NAMESPACE: &str = "rightapp-legal";

/// What the app needs to license: the appKey the licence service knows, its brand host and the Ed25519 key that
/// verifies its tokens. Registered per app (bogusyogi/rightkit#272); never generated.
pub struct LicenseRegistration {
    pub app: &'static str,
    pub host: &'static str,
    pub public_key: [u8; 32],
}

/// `None` until the app is registered: licensing is then disabled (no network, `mode: disabled`) and the app runs.
pub const LICENSE: Option<LicenseRegistration> = None;

/// The generated legal inputs, compiled in: the manifest `legal-snapshot` writes and the acceptance contract the
/// release legal contract checks. The renderer imports the same two files.
const LEGAL_MANIFEST_JSON: &str = include_str!("../../legal/legal-manifest.json");
const LEGAL_CONTRACT_JSON: &str = include_str!("../../src/generated/legal-contract.json");

/// The manifest the receipt service compares a declaration with: the contract's acceptance version and material
/// digest, and every document as `@rightkit/legal`'s `acceptanceDeclaration` lists it (id, role, version, sha256,
/// treatment, material; no title or URL).
pub fn legal_manifest() -> Result<LegalManifest, String> {
    let manifest: Value = serde_json::from_str(LEGAL_MANIFEST_JSON).map_err(|e| e.to_string())?;
    let contract: Value = serde_json::from_str(LEGAL_CONTRACT_JSON).map_err(|e| e.to_string())?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let version =
        text(&contract, "acceptanceVersion").ok_or("legal contract has no acceptanceVersion")?;
    if text(&manifest, "acceptanceVersion").as_deref() != Some(version.as_str()) {
        return Err(
            "legal manifest and contract disagree on the acceptance version; run legal-snapshot"
                .into(),
        );
    }
    let digest = text(&contract, "materialSha256").ok_or("legal contract has no materialSha256")?;
    let documents = manifest
        .get("documents")
        .and_then(Value::as_array)
        .ok_or("legal manifest has no documents")?
        .iter()
        .map(|d| DeclaredDocument {
            id: text(d, "id"),
            role: text(d, "role"),
            version: text(d, "version"),
            sha256: text(d, "sha256"),
            treatment: text(d, "treatment"),
            material: d.get("material").and_then(Value::as_bool),
            ..DeclaredDocument::default()
        })
        .collect();
    LegalManifest::new(version, digest, documents).map_err(|e| e.to_string())
}

/// The settings bridge over `<data root>/settings.json`.
pub fn settings_service(data_root: &Path) -> Result<SettingsHostService, String> {
    let document = Document::open_path(data_root.join("settings.json"), SETTINGS_NAMESPACE)
        .map_err(|e| e.to_string())?;
    let schema = StaticSchema::new(SETTINGS_VERSION).keys(SETTINGS_KEYS);
    Ok(SettingsHostService::new(SettingsBridge::new(
        document, schema,
    )))
}

/// The legal receipt over `<data root>/legal-receipt.json`.
pub fn legal_service(data_root: &Path) -> Result<LegalReceiptService, String> {
    let document = Document::open_path(data_root.join("legal-receipt.json"), LEGAL_NAMESPACE)
        .map_err(|e| e.to_string())?;
    Ok(LegalReceiptService::new(
        legal_manifest()?,
        DocumentReceiptStore::new(document),
    ))
}

/// The device identity, as the licence client binds activations to it.
pub struct DeviceIds(pub Arc<DeviceService<CredentialStore>>);

impl DeviceIdSource for DeviceIds {
    fn device_id(&self) -> Result<String, DeviceIdError> {
        self.0.get_or_create().map_err(|_| DeviceIdError)
    }

    fn device_id_hash(&self) -> Result<String, DeviceIdError> {
        self.device_id()
            .map(|id| rightkit_device::device_id_hash(&id))
    }
}

/// Disabled without a registration; otherwise a client over the app's credential store and its private
/// `<data root>/license` directory (operation lock, clock ledger, rotation locks).
pub fn license_setup(
    registration: Option<&LicenseRegistration>,
    store: Store,
    data_root: &Path,
) -> Result<LicenseSetup, String> {
    let Some(reg) = registration else {
        return Ok(LicenseSetup::Disabled);
    };
    let dir = data_root.join("license");
    rightkit_fs::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
    let config = LicenseClientConfig::new(
        reg.app,
        reg.host,
        env!("CARGO_PKG_VERSION"),
        reg.public_key,
        store,
        dir,
    )
    .map_err(|e| e.to_string())?;
    Ok(LicenseSetup::configured(config))
}

pub fn license_client(
    setup: LicenseSetup,
    device: Arc<DeviceService<CredentialStore>>,
) -> Result<LicenseClient, String> {
    LicenseClient::new(setup, Arc::new(DeviceIds(device))).map_err(|e| e.to_string())
}

/// The app's HTTP client (M17): first-party to the licence host, HTTPS only, with the shared timeouts, bounded
/// bodies and retry policy of `rightkit_http`. Add a host here only for a service the app really calls.
pub struct AppHttp(pub rightkit_http::Client);

pub fn app_http(registration: Option<&LicenseRegistration>) -> Option<AppHttp> {
    let reg = registration?;
    let mut config = rightkit_http::ClientConfig::first_party(vec![reg.host.to_owned()]);
    config.user_agent = format!("RightApp/{}", env!("CARGO_PKG_VERSION"));
    Some(AppHttp(rightkit_http::Client::new(config)))
}

/// Every host service, under one caller policy that trusts [`TRUSTED_WINDOWS`]. Logs are listed too: once host
/// services are installed, the renderer log commands ask this policy.
pub fn host_services(data_root: &Path, license: LicenseClient) -> Result<HostServices, String> {
    let trust = || ServiceTrust::for_labels(TRUSTED_WINDOWS);
    HostServices::builder()
        .settings(settings_service(data_root)?, trust())
        .legal(trust(), legal_service(data_root)?)
        .license_client(trust(), license)
        .logs(trust())
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_tauri_shell::host::LegalCreateRequest;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rightapp-services-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn declaration() -> Value {
        let manifest: Value = serde_json::from_str(LEGAL_MANIFEST_JSON).unwrap();
        let contract: Value = serde_json::from_str(LEGAL_CONTRACT_JSON).unwrap();
        let documents: Vec<Value> = manifest["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                serde_json::json!({
                    "id": d["id"], "role": d["role"], "version": d["version"],
                    "sha256": d["sha256"], "treatment": d["treatment"], "material": d["material"],
                })
            })
            .collect();
        serde_json::json!({
            "version": contract["acceptanceVersion"],
            "documentsHash": contract["materialSha256"],
            "documents": documents,
            "eligibilityBasis": "individual",
            "surface": "first_run",
        })
    }

    // rk-test: pure
    #[test]
    fn the_compiled_legal_manifest_is_the_generated_contract() {
        let manifest = legal_manifest().expect("legal manifest");
        let contract: Value = serde_json::from_str(LEGAL_CONTRACT_JSON).unwrap();
        assert_eq!(
            manifest.version(),
            contract["acceptanceVersion"].as_str().unwrap()
        );
        assert_eq!(
            manifest.documents_hash(),
            contract["materialSha256"].as_str().unwrap()
        );
    }

    // rk-test: pure
    #[test]
    fn the_receipt_records_the_declaration_the_gate_sends_and_refuses_another_version() {
        let root = scratch("legal");
        let service = legal_service(&root).unwrap();
        assert_eq!(service.read().unwrap(), None);
        let mut stale = declaration();
        stale["version"] = Value::from("rightapp-1999-01-01-v0");
        let stale: LegalCreateRequest = serde_json::from_value(stale).unwrap();
        assert!(
            service.create(stale).is_err(),
            "a declaration for another version is refused"
        );
        let request: LegalCreateRequest = serde_json::from_value(declaration()).unwrap();
        let record = service
            .create(request)
            .expect("the gate's declaration matches the manifest");
        assert_eq!(service.read().unwrap(), Some(record));
        std::fs::remove_dir_all(root).ok();
    }

    // rk-test: pure
    #[test]
    fn settings_accept_the_onboarding_state_and_refuse_an_undeclared_key() {
        let root = scratch("settings");
        let service = settings_service(&root).unwrap();
        let bridge = service.bridge();
        assert!(bridge
            .patch(
                &serde_json::json!({ "onboarding.state": { "currentStep": 0 } }),
                None
            )
            .is_ok());
        assert!(bridge
            .patch(&serde_json::json!({ "anything.else": 1 }), None)
            .is_err());
        std::fs::remove_dir_all(root).ok();
    }

    // rk-test: pure
    #[test]
    fn without_a_registration_licensing_is_disabled_and_there_is_no_http_client() {
        let root = scratch("license");
        let store = Store::open(rightkit_credentials::Config::new(
            rightkit_credentials::Namespace::new("app.rightsuite.rightapp.test").unwrap(),
        ));
        let setup = license_setup(LICENSE.as_ref(), store, &root).unwrap();
        assert!(matches!(setup, LicenseSetup::Disabled));
        assert!(app_http(LICENSE.as_ref()).is_none());
        assert!(
            !root.join("license").exists(),
            "a disabled setup creates nothing"
        );
        std::fs::remove_dir_all(root).ok();
    }

    // rk-test: pure
    #[test]
    fn the_app_http_client_reaches_only_the_licence_host_over_https() {
        let reg = LicenseRegistration {
            app: "rightapp",
            host: "licence.rightapp.example",
            public_key: [7; 32],
        };
        let AppHttp(client) = app_http(Some(&reg)).unwrap();
        for url in [
            "https://elsewhere.example/v1",
            "http://licence.rightapp.example/v1",
        ] {
            let err = client.send(&rightkit_http::Request::get(url)).unwrap_err();
            assert!(
                matches!(err, rightkit_http::HttpError::AddressDenied { .. }),
                "{url}: {err}"
            );
        }
    }
}
