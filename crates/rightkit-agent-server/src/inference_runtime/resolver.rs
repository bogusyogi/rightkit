use super::{
    catalog_entry_hash, selection_fingerprint, DirectAuthKind, DirectConnection,
    InferenceAuthority, InferenceRouteError, InferenceSelection, ResolvedInferenceRoute,
    SelectionFingerprintInput,
};
use crate::api::DaemonState;
use rightkit_agent_engine_storage::InferenceRouteAttestationRow;

fn hash(value: &str) -> String {
    rightkit_fs::digest::sha256_bytes(value.as_bytes())
}

fn protocol_name(selection: &InferenceSelection) -> &'static str {
    match selection.protocol {
        super::InferenceProtocol::OpenaiCompatible => "openai_compatible",
        super::InferenceProtocol::AnthropicCompatible => "anthropic_compatible",
    }
}

/// Resolve exactly one selection before any request. No legacy provider/model
/// string is accepted here, so unknown or stale selections fail before HTTP.
pub async fn ensure_inference_selection_qualified(
    state: &DaemonState,
    selection: &InferenceSelection,
) -> Result<ResolvedInferenceRoute, InferenceRouteError> {
    selection
        .validate()
        .map_err(|_| InferenceRouteError::InvalidSelection)?;
    let storage = state
        .storage
        .clone()
        .ok_or(InferenceRouteError::AuthorityUnavailable)?;
    let (
        endpoint,
        auth_header_name,
        auth_secret,
        public_headers,
        catalog_hash,
        configuration_revision,
        credential_revision,
        direct_connection,
    ) = match selection.authority {
        InferenceAuthority::Direct => {
            let row = storage
                .lock()
                .await
                .direct_inference_connection(&selection.connection_id)
                .map_err(|_| InferenceRouteError::AuthorityUnavailable)?
                .ok_or(InferenceRouteError::ConnectionNotFound)?;
            let protocol: super::InferenceProtocol =
                serde_json::from_str(&format!("\"{}\"", row.protocol))
                    .map_err(|_| InferenceRouteError::InvalidSelection)?;
            if protocol != selection.protocol {
                return Err(InferenceRouteError::InvalidSelection);
            }
            let auth_kind: DirectAuthKind = serde_json::from_str(&format!("\"{}\"", row.auth_kind))
                .map_err(|_| InferenceRouteError::InvalidSelection)?;
            let connection = DirectConnection {
                id: row.id.clone(),
                name: row.name,
                protocol,
                normalized_base_url: row.base_url.clone(),
                auth_kind,
                auth_header_name: row.auth_header_name.clone(),
                public_headers: serde_json::from_str(&row.public_headers_json)
                    .map_err(|_| InferenceRouteError::InvalidSelection)?,
                manual_model_ids: serde_json::from_str(&row.manual_models_json)
                    .map_err(|_| InferenceRouteError::InvalidSelection)?,
                trust_private_network: row.trust_private_network,
                credential_configured: row.credential_configured,
                credential_revision: row.credential_revision,
                configuration_revision: row.configuration_revision,
                created_at: row.created_at,
                updated_at: row.updated_at,
            };
            let (header, secret) = match auth_kind {
                DirectAuthKind::None => (None, None),
                kind => {
                    if !connection.credential_configured {
                        return Err(InferenceRouteError::SecretRequired);
                    }
                    let secret = rightkit_llm::agent_providers::KeyStore::get(
                        crate::host_identity::app(),
                        &format!("direct-connection:{}", connection.id),
                    )
                    .map_err(|_| InferenceRouteError::SecretRequired)?;
                    let header = match kind {
                        DirectAuthKind::Bearer => "Authorization".into(),
                        DirectAuthKind::XApiKey => "x-api-key".into(),
                        DirectAuthKind::ApiKeyHeader => connection
                            .auth_header_name
                            .clone()
                            .ok_or(InferenceRouteError::InvalidSelection)?,
                        DirectAuthKind::None => unreachable!(),
                    };
                    (Some(header), Some(secret))
                }
            };
            let request_auth_header = header.as_ref().and_then(|name| {
                secret.as_ref().map(|secret| {
                    let value = if name.eq_ignore_ascii_case("authorization") {
                        format!("Bearer {secret}")
                    } else {
                        secret.clone()
                    };
                    (name.clone(), value)
                })
            });
            let model = super::discover_direct_models(&connection, request_auth_header)
                .await
                .map_err(|code| match code {
                    "model_unavailable" => InferenceRouteError::ModelUnavailable,
                    "secret_required" => InferenceRouteError::SecretRequired,
                    _ => InferenceRouteError::AuthorityUnavailable,
                })?
                .into_iter()
                .find(|model| model.id == selection.model_id)
                .ok_or(InferenceRouteError::ModelUnavailable)?;
            (
                connection.normalized_base_url.clone(),
                header,
                secret,
                connection.public_headers.clone(),
                catalog_entry_hash(&model),
                connection.configuration_revision,
                connection.credential_revision,
                Some(connection),
            )
        }
    };
    let authority = match selection.authority {
        InferenceAuthority::Direct => "direct",
    };
    let fingerprint = selection_fingerprint(&SelectionFingerprintInput {
        schema: selection.schema_version,
        authority,
        connection_id: &selection.connection_id,
        protocol: protocol_name(selection),
        normalized_base_url: &endpoint,
        auth_kind: auth_header_name.as_deref().unwrap_or("none"),
        auth_header_name: auth_header_name.as_deref(),
        credential_revision,
        configuration_revision,
        model_id: &selection.model_id,
        route_policy_id: selection.route_policy_id.as_deref(),
        catalog_entry_hash: &catalog_hash,
        security_policy_hash: &hash(&crate::host_identity::prefixed("direct-security-v1")),
        tool_schema_hash: &hash(&crate::host_identity::prefixed("local-tools-v1")),
    });
    let attestation_id = format!("att-{}", &fingerprint[..24]);
    let row = InferenceRouteAttestationRow {
        id: attestation_id.clone(),
        authority: authority.into(),
        connection_id: selection.connection_id.clone(),
        protocol: protocol_name(selection).into(),
        model_id: selection.model_id.clone(),
        route_policy_id: selection.route_policy_id.clone(),
        selection_fingerprint: fingerprint.clone(),
        catalog_entry_hash: catalog_hash,
        security_policy_hash: hash(&crate::host_identity::prefixed("direct-security-v1")),
        tool_schema_hash: hash(&crate::host_identity::prefixed("local-tools-v1")),
        configuration_revision,
        credential_revision,
        status: "qualified".into(),
        verified_at: crate::api::now_iso(),
    };
    storage
        .lock()
        .await
        .upsert_inference_route_attestation(&row)
        .map_err(|_| InferenceRouteError::AttestationRequired)?;
    Ok(ResolvedInferenceRoute {
        selection: selection.clone(),
        fingerprint,
        endpoint,
        auth_header_name,
        auth_secret,
        public_headers,
        direct_connection,
        attestation_id,
    })
}

/// Persist a root or child binding only after its route has qualified. Resume
/// compares this fingerprint before rebuilding an engine, refusing drift.
pub async fn bind_inference_route(
    state: &DaemonState,
    session_id: &str,
    agent_id: &str,
    parent_agent_id: Option<&str>,
    route: &ResolvedInferenceRoute,
) -> Result<(), InferenceRouteError> {
    let storage = state
        .storage
        .clone()
        .ok_or(InferenceRouteError::AuthorityUnavailable)?;
    let authority = match route.selection.authority {
        InferenceAuthority::Direct => "direct",
    };
    let now = crate::api::now_iso();
    let row = rightkit_agent_engine_storage::InferenceRuntimeBindingRow {
        id: format!("binding-{}-{}", session_id, agent_id),
        session_id: session_id.into(),
        agent_id: agent_id.into(),
        parent_agent_id: parent_agent_id.map(str::to_owned),
        authority: authority.into(),
        connection_id: route.selection.connection_id.clone(),
        protocol: protocol_name(&route.selection).into(),
        model_id: route.selection.model_id.clone(),
        route_policy_id: route.selection.route_policy_id.clone(),
        selection_fingerprint: route.fingerprint.clone(),
        attestation_id: route.attestation_id.clone(),
        status: "bound".into(),
        created_at: now.clone(),
        updated_at: now,
    };
    let result = storage
        .lock()
        .await
        .upsert_inference_runtime_binding(&row)
        .map_err(|_| InferenceRouteError::AttestationRequired);
    result
}

pub async fn resume_binding_matches(
    state: &DaemonState,
    session_id: &str,
    route: &ResolvedInferenceRoute,
) -> Result<(), InferenceRouteError> {
    let storage = state
        .storage
        .clone()
        .ok_or(InferenceRouteError::AuthorityUnavailable)?;
    let binding = storage
        .lock()
        .await
        .inference_runtime_binding(session_id, session_id)
        .map_err(|_| InferenceRouteError::AttestationRequired)?
        .ok_or(InferenceRouteError::AttestationRequired)?;
    (binding.selection_fingerprint == route.fingerprint
        && binding.attestation_id == route.attestation_id)
        .then_some(())
        .ok_or(InferenceRouteError::AttestationRequired)
}
