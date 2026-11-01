//! Host-owned retained-context facts.
//!
//! Membrane chooses representation. RightKit records only what its host
//! actually retained after serialization, then invalidates it on lifecycle
//! changes. No identity or representation is inferred here.

use crate::membrane_binding::host_observation::{
    HostReceiptProvenanceV1, LoadedContextIdentitiesV1, LoadedContextIdentityV1, ObservedFieldV1,
    PacketDeliveryAcknowledgementStatusV1, PacketDeliveryAcknowledgementV1, RepresentationHandleV1,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct HostRetainedContextV1 {
    pub loaded: LoadedContextIdentitiesV1,
    pub acknowledgement: PacketDeliveryAcknowledgementV1,
}

/// Record exact provider-visible bytes only after host context assembly has
/// accepted them. The handle is parsed from Membrane output and never rebuilt.
pub(crate) fn retain_serialized_membrane_context(
    response: &Value,
    serialized_bytes: &[u8],
) -> Result<(RepresentationHandleV1, String), &'static str> {
    if serialized_bytes.is_empty() {
        return Err("cannot retain empty serialized context");
    }
    let handle: RepresentationHandleV1 = serde_json::from_value(
        response
            .get("representationHandle")
            .cloned()
            .ok_or("missing Membrane representation handle")?,
    )
    .map_err(|_| "invalid Membrane representation handle")?;
    handle
        .validate()
        .map_err(|_| "invalid Membrane representation handle")?;
    let mut digest = rightkit_fs::digest::Sha256Hasher::new();
    digest.update(serialized_bytes);
    Ok((handle, format!("sha256:{}", digest.finalize())))
}

/// Build the host callback at the context-assembly boundary.  `retained_bytes`
/// is the host-bound serialization of the exact Membrane response retained
/// from authorization; no identity or digest is derived from federation
/// metadata. `packet_digest` is the emission digest carried on the federation
/// result — computed over the packet exactly as Membrane emitted it, before
/// the host delivery projection removed or stripped any block. Re-digesting
/// the projected `response.packet` here would never receipt-match the pending
/// Pull publication.
pub(crate) fn callback_for_retained_context(
    retained_bytes: &[u8],
    session_id: &str,
    task_id: &str,
    authorization_id: &str,
    observed_at_unix_ms: u64,
    packet_digest: Option<&str>,
) -> Result<crate::observation::HostRetentionCallbackV1, &'static str> {
    let serialized_bytes = retained_bytes;
    let retained_response: Value = serde_json::from_slice(serialized_bytes)
        .map_err(|_| "retained Membrane context was not valid JSON")?;
    let (handle, host_digest) =
        retain_serialized_membrane_context(&retained_response, serialized_bytes)?;
    let packet_digest = packet_digest
        .filter(|digest| {
            digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .ok_or("Membrane response omitted a valid emission packet digest")?
        .to_owned();
    let receipt = HostReceiptProvenanceV1::new(
        authorization_id,
        "coderight.provider.context-assembly",
        observed_at_unix_ms,
        host_digest.clone(),
    );
    let loaded = LoadedContextIdentitiesV1 {
        schema_version: 1,
        snapshot_id: format!("{authorization_id}:retained"),
        session_id: session_id.to_owned(),
        compaction_generation: ObservedFieldV1::complete(handle.context_epoch),
        identities: ObservedFieldV1::complete(vec![LoadedContextIdentityV1 {
            identity: handle.handle,
            source_ref: handle.source_ref,
            source_digest: handle.source_digest.clone(),
        }]),
        observed_at_unix_ms,
        provenance_receipt: receipt.clone(),
    };
    let acknowledgement = PacketDeliveryAcknowledgementV1 {
        schema_version: 1,
        acknowledgement_id: format!("{authorization_id}:ack"),
        packet_digest,
        host_serialized_digest: host_digest,
        session_id: session_id.to_owned(),
        task_id: ObservedFieldV1::complete(task_id.to_owned()),
        status: PacketDeliveryAcknowledgementStatusV1::Acknowledged,
        serialized_bytes: ObservedFieldV1::complete(serialized_bytes.len() as u64),
        acknowledged_at_unix_ms: observed_at_unix_ms,
        provenance_receipt: receipt,
    };
    Ok(crate::observation::HostRetentionCallbackV1 {
        loaded,
        acknowledgement,
    })
}

/// The retained representation handle on the authorized Membrane response —
/// minted by Membrane, read back verbatim after host serialization.
pub(crate) fn retained_representation_handle(response: &Value) -> Result<Value, &'static str> {
    response
        .get("representationHandle")
        .cloned()
        .ok_or("retained Membrane context omitted representation handle")
}

impl HostRetainedContextV1 {
    pub(crate) fn validate(&self, session_id: &str) -> Result<(), &'static str> {
        self.loaded.validate().map_err(|_| "invalid retained H9")?;
        self.acknowledgement
            .validate()
            .map_err(|_| "invalid retained H10")?;
        if self.loaded.session_id != session_id
            || self.acknowledgement.session_id != session_id
            || self.acknowledgement.serialized_bytes.value.unwrap_or(0) == 0
        {
            return Err("retained context identity mismatch");
        }
        Ok(())
    }

    pub(crate) fn invalidate(self) -> HostRetainedContextInvalidationV1 {
        HostRetainedContextInvalidationV1 {
            previous_epoch: self.loaded.compaction_generation.value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostRetainedContextInvalidationV1 {
    pub previous_epoch: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "handle": "opaque-handle",
            "class": "rendered_full",
            "installationId": "install",
            "contextEpoch": 1,
            "sourceRef": "packet://trace-1",
            "sourceDigest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        })
    }

    #[test]
    fn retained_response_bytes_digest_stably() {
        let response = serde_json::json!({
            "representationHandle": handle(),
            "packet": {"blocks": []}
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let (handle_a, digest_a) =
            retain_serialized_membrane_context(&response, &bytes).expect("valid retention");
        let (_, digest_b) =
            retain_serialized_membrane_context(&response, &bytes).expect("stable digest");
        assert_eq!(digest_a, digest_b);
        assert_eq!(handle_a.handle, "opaque-handle");
    }

    #[test]
    fn missing_representation_handle_is_refused() {
        let response = serde_json::json!({"packet": {"blocks": []}});
        let bytes = serde_json::to_vec(&response).unwrap();
        assert!(retain_serialized_membrane_context(&response, &bytes).is_err());
        assert!(retained_representation_handle(&response).is_err());
    }

    #[test]
    fn empty_retained_bytes_are_refused() {
        let response = serde_json::json!({"representationHandle": handle()});
        assert!(retain_serialized_membrane_context(&response, &[]).is_err());
    }
}
