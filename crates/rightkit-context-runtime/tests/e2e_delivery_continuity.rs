//! End-to-end: packet delivery acknowledgement and loaded-identity sync across
//! host compaction, over the real on-disk context catalog (reopened between
//! steps, as a worker restart would).

use rightkit_context_protocol::{
    HostObservationProvenanceV1, LoadedContextIdentitiesV1, LoadedContextIdentityV1,
    ObservedFieldV1, PacketDeliveryAcknowledgementStatusV1, PacketDeliveryAcknowledgementV1,
    RepresentationClassV1, RepresentationHandleV1, HOST_OBSERVATION_SCHEMA_VERSION,
    LOADED_CONTEXT_IDENTITIES_SCHEMA_VERSION, PACKET_DELIVERY_ACKNOWLEDGEMENT_SCHEMA_VERSION,
};
use rightkit_context_runtime::catalog::{
    record_pending_pull_publication, ContextCatalog, PendingPullPublicationV1,
};
use rightkit_context_runtime::pull::delivery_acknowledgement::{
    acknowledge_delivery, suppression_eligible,
};

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn publication() -> PendingPullPublicationV1 {
    PendingPullPublicationV1 {
        repository_id: "repo-1".into(),
        request_id: "request-1".into(),
        trace_id: "trace-1".into(),
        task_id: "task-1".into(),
        session_id: "session-1".into(),
        context_epoch: 7,
        publication_id: "publication-1".into(),
        representation_identity: "evidence-1".into(),
        source_ref: "source-1".into(),
        representation_digest: digest('a'),
        packet_digest: digest('b'),
    }
}

fn provenance() -> HostObservationProvenanceV1 {
    HostObservationProvenanceV1::new("receipt-1", "coderight", 10, digest('c'))
}

fn loaded(publication: &PendingPullPublicationV1, epoch: u64) -> LoadedContextIdentitiesV1 {
    LoadedContextIdentitiesV1 {
        schema_version: LOADED_CONTEXT_IDENTITIES_SCHEMA_VERSION,
        snapshot_id: "snapshot-1".into(),
        session_id: publication.session_id.clone(),
        compaction_generation: ObservedFieldV1::complete(epoch),
        identities: ObservedFieldV1::complete(vec![LoadedContextIdentityV1 {
            identity: publication.representation_identity.clone(),
            source_ref: publication.source_ref.clone(),
            source_digest: publication.representation_digest.clone(),
        }]),
        observed_at_unix_ms: 20,
        provenance_receipt: provenance(),
    }
}

fn acknowledgement(publication: &PendingPullPublicationV1) -> PacketDeliveryAcknowledgementV1 {
    PacketDeliveryAcknowledgementV1 {
        schema_version: PACKET_DELIVERY_ACKNOWLEDGEMENT_SCHEMA_VERSION,
        acknowledgement_id: "ack-1".into(),
        packet_digest: publication.packet_digest.clone(),
        host_serialized_digest: digest('c'),
        session_id: publication.session_id.clone(),
        task_id: ObservedFieldV1::complete(publication.task_id.clone()),
        status: PacketDeliveryAcknowledgementStatusV1::Acknowledged,
        serialized_bytes: ObservedFieldV1::complete(1),
        acknowledged_at_unix_ms: 11,
        provenance_receipt: provenance(),
    }
}

#[test]
fn acknowledgement_needs_a_pending_publication_and_exact_host_retention_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let publication = publication();
    let retained = loaded(&publication, publication.context_epoch);
    let ack = acknowledgement(&publication);

    let catalog = ContextCatalog::open(&path).unwrap();
    // Nothing was published yet: an acknowledgement cannot create delivery truth.
    assert!(acknowledge_delivery(&catalog, &publication, &ack, &retained).is_err());
    record_pending_pull_publication(&catalog, &publication).unwrap();

    // A rejected, mismatched-digest or other-task acknowledgement is refused.
    let mut rejected = ack.clone();
    rejected.status = PacketDeliveryAcknowledgementStatusV1::Rejected;
    assert!(acknowledge_delivery(&catalog, &publication, &rejected, &retained).is_err());
    let mut tampered = ack.clone();
    tampered.host_serialized_digest = digest('d');
    assert!(acknowledge_delivery(&catalog, &publication, &tampered, &retained).is_err());
    let mut wrong_task = ack.clone();
    wrong_task.task_id = ObservedFieldV1::complete("other-task".into());
    assert!(acknowledge_delivery(&catalog, &publication, &wrong_task, &retained).is_err());

    // The exact acknowledgement lands and makes the evidence suppressible.
    assert!(acknowledge_delivery(&catalog, &publication, &ack, &retained).unwrap());
    assert!(suppression_eligible(&catalog, &publication, &retained).unwrap());
    drop(catalog);

    // Durable: a reopened catalog still honors it, and still refuses tampering.
    let reopened = ContextCatalog::open(&path).unwrap();
    assert!(suppression_eligible(&reopened, &publication, &retained).unwrap());
    assert!(acknowledge_delivery(&reopened, &publication, &tampered, &retained).is_err());
}

#[test]
fn host_compaction_revokes_suppression_until_the_identity_is_loaded_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let publication = publication();
    let retained = loaded(&publication, publication.context_epoch);

    let first = ContextCatalog::open(&path).unwrap();
    record_pending_pull_publication(&first, &publication).unwrap();
    acknowledge_delivery(
        &first,
        &publication,
        &acknowledgement(&publication),
        &retained,
    )
    .unwrap();
    drop(first);

    let reopened = ContextCatalog::open(&path).unwrap();
    assert!(suppression_eligible(&reopened, &publication, &retained).unwrap());
    // The host compacted (new generation): the old delivery proves nothing.
    assert!(!suppression_eligible(&reopened, &publication, &loaded(&publication, 8)).unwrap());
    // Same epoch but the host no longer reports the identity loaded.
    let mut dropped = loaded(&publication, publication.context_epoch);
    dropped.identities = ObservedFieldV1::complete(Vec::new());
    assert!(!suppression_eligible(&reopened, &publication, &dropped).unwrap());
    // Reloading the identity at the original generation restores eligibility.
    assert!(suppression_eligible(&reopened, &publication, &retained).unwrap());
}

#[test]
fn canonical_representation_handles_bind_to_the_catalog_installation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let catalog = ContextCatalog::open(&path).unwrap();
    let mut publication = publication();
    let handle = RepresentationHandleV1 {
        schema_version: HOST_OBSERVATION_SCHEMA_VERSION,
        handle: "opaque-handle".into(),
        class: RepresentationClassV1::RenderedFull,
        installation_id: catalog.startup_report().catalog_installation_id.clone(),
        context_epoch: publication.context_epoch,
        source_ref: publication.source_ref.clone(),
        source_digest: publication.representation_digest.clone(),
    };
    publication.representation_identity = serde_json::to_string(&handle).unwrap();
    let mut retained = loaded(&publication, publication.context_epoch);
    retained.identities.value.as_mut().unwrap()[0].identity = handle.handle.clone();
    record_pending_pull_publication(&catalog, &publication).unwrap();
    assert!(acknowledge_delivery(
        &catalog,
        &publication,
        &acknowledgement(&publication),
        &retained
    )
    .unwrap());
    assert!(suppression_eligible(&catalog, &publication, &retained).unwrap());
}
