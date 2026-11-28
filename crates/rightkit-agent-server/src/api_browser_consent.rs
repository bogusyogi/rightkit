//! Browser consent routes (docs/contracts/agent-browser.md R3, R22, R23, R25).
//!
//! The user (desktop or CLI) grants, lists and revokes the consent that lets the agent
//! browser reuse their signed-in session. The model has no route here and no tool that
//! grants consent: it can only name an existing active receipt in `browser_open`.
//! Receipts and imported profile copies live in `<state_root>/browser-consent/`.
//!
//! The routes are mounted only on a host that turns `host_options::browser_consent_routes` on.
//! Granting and revoking need a trusted source (`enforce_trusted_source_authority`); a guest
//! link reaches none of these paths.

use crate::api_permissions::enforce_trusted_source_authority;
use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_coding_tools::browser_consent::{
    consent_sources, BrowserKind, ConsentStore, UserConsentReceipt,
};
use rightkit_agent_skills::{CommandAction, CommandResult};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::DaemonState;

type RouteResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({"code": code, "message": message.into()})),
    )
}

fn store() -> Result<ConsentStore, (StatusCode, Json<Value>)> {
    ConsentStore::product()
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable", e))
}

fn untrusted(headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(headers).map_err(|status| {
        error(
            status,
            "untrusted_source",
            "browser consent can only be changed from a trusted source",
        )
    })
}

fn receipt_json(receipt: &UserConsentReceipt) -> Value {
    json!({
        "id": receipt.id,
        "browser": receipt.browser.name(),
        "profile": receipt.profile,
        "domains": receipt.domains,
        "grantedAtMs": receipt.granted_at_ms,
        "expiresAtMs": receipt.expires_at_ms,
        "revokedAtMs": receipt.revoked_at_ms,
        "active": receipt.check_active().is_ok(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantRequest {
    browser: String,
    profile: String,
    domains: Vec<String>,
    #[serde(default)]
    expires_at_ms: Option<u64>,
}

fn parse_browser(name: &str) -> Option<BrowserKind> {
    match name {
        "chrome" => Some(BrowserKind::Chrome),
        "edge" => Some(BrowserKind::Edge),
        _ => None,
    }
}

/// What the consent screen shows before anything is read: the default browser and the
/// profiles it could offer. Listing profiles reads only the browser's `Local State` names.
async fn sources_handler() -> RouteResult {
    let (default, profiles) = consent_sources();
    Ok(Json(json!({
        "defaultBrowser": default.name(),
        "supported": matches!(default, BrowserKind::Chrome | BrowserKind::Edge),
        "profiles": profiles
            .into_iter()
            .map(|(dir, display)| json!({"dirName": dir, "displayName": display}))
            .collect::<Vec<_>>(),
    })))
}

async fn list_handler() -> RouteResult {
    let (receipts, broken) = store()?
        .list()
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, "consent_unreadable", e))?;
    Ok(Json(json!({
        "consents": receipts.iter().map(receipt_json).collect::<Vec<_>>(),
        "broken": broken
            .iter()
            .map(|(path, why)| json!({"path": path.display().to_string(), "error": why}))
            .collect::<Vec<_>>(),
    })))
}

async fn grant_handler(headers: HeaderMap, Json(req): Json<GrantRequest>) -> RouteResult {
    untrusted(&headers)?;
    let Some(browser) = parse_browser(&req.browser) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "unsupported_browser",
            "browser must be chrome or edge",
        ));
    };
    let receipt = store()?
        .grant(browser, &req.profile, req.domains, req.expires_at_ms)
        .map_err(|e| error(StatusCode::BAD_REQUEST, "consent_invalid", e))?;
    Ok(Json(receipt_json(&receipt)))
}

async fn revoke_handler(headers: HeaderMap, Path(id): Path<String>) -> RouteResult {
    untrusted(&headers)?;
    let store = store()?;
    if store.get(&id).is_err() {
        return Err(error(
            StatusCode::NOT_FOUND,
            "consent_not_found",
            format!("no browser consent '{id}'"),
        ));
    }
    let receipt = store
        .revoke(&id)
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, "revoke_failed", e))?;
    Ok(Json(receipt_json(&receipt)))
}

const CONSENT_USAGE: &str = "usage: /browser-consent [list|sources|grant <chrome|edge> <domain[,domain...]> [profile]|revoke <id>]";

fn ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn fail(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}

/// `/browser-consent`: the daemon-side executor behind the TUI consent screen. It runs in the
/// daemon process so `ConsentStore::revoke` reaches the live sessions in this process's table.
/// It never prints cookie values or profile paths.
pub(crate) async fn command_browser_consent(args: &[String]) -> CommandResult {
    let sub = args.first().map(|a| a.to_lowercase());
    let store = match ConsentStore::product() {
        Ok(store) => store,
        Err(e) => return fail(e),
    };
    match sub.as_deref() {
        None | Some("list") => {
            let (receipts, broken) = match store.list() {
                Ok(listed) => listed,
                Err(e) => return fail(e),
            };
            if receipts.is_empty() && broken.is_empty() {
                return ok("No browser consents.");
            }
            let mut out = String::new();
            if receipts.is_empty() {
                out.push_str("No browser consents.\n");
            } else {
                out.push_str("| id | browser | profile | domains | state |\n");
                out.push_str("| --- | --- | --- | --- | --- |\n");
                for receipt in &receipts {
                    let state = if receipt.revoked_at_ms.is_some() {
                        "revoked"
                    } else if receipt.check_active().is_ok() {
                        "active"
                    } else {
                        "expired"
                    };
                    let domains = match &receipt.domains {
                        Some(domains) => domains.join(", "),
                        None => "every domain".to_string(),
                    };
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {} |\n",
                        receipt.id,
                        receipt.browser.name(),
                        receipt.profile,
                        domains,
                        state
                    ));
                }
            }
            for (path, _) in &broken {
                out.push_str(&format!("unreadable receipt: {}\n", path.display()));
            }
            ok(out.trim_end().to_string())
        }
        Some("sources") => {
            let (default, profiles) = consent_sources();
            let mut out = if matches!(default, BrowserKind::Chrome | BrowserKind::Edge) {
                format!("Default browser: {} (supported)", default.name())
            } else {
                format!(
                    "Default browser: {} (not supported: Chromium-family only)",
                    default.name()
                )
            };
            for (dir, display) in profiles {
                out.push_str(&format!("\n{dir} ({display})"));
            }
            ok(out)
        }
        Some("grant") => {
            let Some(browser) = args
                .get(1)
                .and_then(|name| parse_browser(&name.to_lowercase()))
            else {
                return fail(if args.len() < 2 {
                    CONSENT_USAGE.to_string()
                } else {
                    "browser must be chrome or edge".to_string()
                });
            };
            let Some(domain_arg) = args.get(2) else {
                return fail(CONSENT_USAGE);
            };
            let domains: Vec<String> = domain_arg
                .split(',')
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect();
            let profile = if args.len() > 3 {
                args[3..].join(" ")
            } else {
                "Default".to_string()
            };
            match store.grant(browser, &profile, domains, None) {
                Ok(receipt) => ok(format!(
                    "Granted {}: {} profile {} for {}. Revoke with /browser-consent revoke {}.",
                    receipt.id,
                    browser.name(),
                    profile,
                    receipt.domains.clone().unwrap_or_default().join(", "),
                    receipt.id
                )),
                Err(e) => fail(e),
            }
        }
        Some("revoke") => {
            let Some(id) = args.get(1) else {
                return fail(CONSENT_USAGE);
            };
            match store.revoke(id).await {
                Ok(_) => ok(format!(
                    "Revoked {id}; sessions stopped and the imported copy purged."
                )),
                Err(e) => fail(e),
            }
        }
        Some(_) => fail(CONSENT_USAGE),
    }
}

pub fn browser_consent_routes() -> Router<DaemonState> {
    Router::new()
        .route("/browser/consent/sources", get(sources_handler))
        .route("/browser/consents", get(list_handler).post(grant_handler))
        .route("/browser/consents/{id}/revoke", post(revoke_handler))
}
