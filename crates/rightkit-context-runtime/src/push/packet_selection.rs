//! Materialization and measurement under one planner-owned capacity observation.
use super::prep::{is_code_ext, is_structured_text, PushPolicy};
use super::selection::{
    PacketReductionRequestError, PacketReductionSelectionV1, RequestTimeH8Error,
};
use super::telemetry;
use rightkit_context_protocol::host_observation::RemainingContextCeilingV1;
use rightkit_context_protocol::push::{
    PacketReductionPlanV1, PacketReductionRepresentationV1, PacketReductionSelectionError,
    PacketReductionSelectionReceiptV1, PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION,
};
use rightkit_memory_core::planner::ContextPacketV1;
use serde_json::Value;
use std::path::Path;

/// Count the exact serialized packet data using the named tokenizer. Unknown
/// host counters are refused; copying a host label is never a measurement.
pub fn measure_packet(
    value: &Value,
    basis: &rightkit_context_protocol::host_observation::EstimatorBasisV1,
) -> Result<u64, PacketReductionRequestError> {
    if basis.id != "o200k_base" || basis.version != "1" {
        return Err(PacketReductionRequestError::H8(
            RequestTimeH8Error::Invalid(
                "unsupported estimator basis; expected o200k_base/1".into(),
            ),
        ));
    }
    rightkit_memory_core::ContextTokenAccounting::count_exact(&value.to_string())
        .map(|n| n as u64)
        .map_err(PacketReductionRequestError::ContentSerialization)
}

/// The text an explicit pull delivers to its consumer for one packet representation: every
/// block's text, in packet order, joined by a blank line. This is exactly the `context` string
/// `membrane context pull` prints (and `--json` carries as `context`) and the string
/// `hook::packet_text` extracts from a packet. Recovery handles and omission markers a
/// representation renders are part of a block's text and so are counted here; the packet's
/// metadata, provider accounting and omission records are diagnostics (`detail`), not delivered
/// text. `None` when the value has no `blocks` array.
pub fn consumer_visible_text(packet: &Value) -> Option<String> {
    let blocks = packet.get("blocks").and_then(Value::as_array)?;
    Some(
        blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Host-proven resolver capability, scoped by the already-authorized caller.
pub struct RecoveryContext<'a> {
    pub store: &'a super::recovery::RecoveryStore,
    pub scope: &'a super::recovery::RecoveryScope,
    pub resolver_token: &'a str,
}

pub fn build_packet_reduction_plan(
    packet: &ContextPacketV1,
    basis: rightkit_context_protocol::host_observation::EstimatorBasisV1,
) -> Result<PacketReductionPlanV1, PacketReductionRequestError> {
    build_packet_reduction_plan_with_policy(packet, basis, &PushPolicy::Control)
}
pub fn build_packet_reduction_plan_with_policy(
    packet: &ContextPacketV1,
    basis: rightkit_context_protocol::host_observation::EstimatorBasisV1,
    policy: &PushPolicy,
) -> Result<PacketReductionPlanV1, PacketReductionRequestError> {
    // Pure callers cannot claim recovery for an unretained input.
    build_with_recovery(packet, basis, policy, None)
}
fn build_with_recovery(
    packet: &ContextPacketV1,
    basis: rightkit_context_protocol::host_observation::EstimatorBasisV1,
    policy: &PushPolicy,
    recovery: Option<&RecoveryContext<'_>>,
) -> Result<PacketReductionPlanV1, PacketReductionRequestError> {
    if packet.trace_id.trim().is_empty() {
        return Err(PacketReductionRequestError::EmptyPacket("traceId"));
    }
    if packet.blocks.is_empty() {
        return Err(PacketReductionRequestError::EmptyPacket("blocks"));
    }
    if packet.blocks.len() > 4096
        || packet.blocks.iter().map(|b| b.text.len()).sum::<usize>()
            > super::recovery::MAX_ARTIFACT_BYTES
    {
        return Err(PacketReductionRequestError::ContentSerialization(
            "push_packet_limit".into(),
        ));
    }
    let mut protected = Vec::new();
    let mut all_ids = Vec::new();
    for block in &packet.blocks {
        for (field, value) in [("id", &block.id), ("resolver", &block.resolver)] {
            if value.trim().is_empty() {
                return Err(PacketReductionRequestError::EmptyBlockField {
                    block: block.id.clone(),
                    field,
                });
            }
        }
        if all_ids.contains(&block.id) {
            return Err(PacketReductionRequestError::ContentSerialization(
                "duplicate block identity".into(),
            ));
        }
        all_ids.push(block.id.clone());
        if block.protected {
            protected.push(block.id.clone());
        }
    }
    // Ordinary retrieval may contain no protected blocks. The empty protected
    // set is valid; exact measurement, recovery & host capacity still apply.
    // Refused query policy is terminal. The public opt-in cannot mint proof.
    let policy_admitted = match policy {
        PushPolicy::Control => true,
        PushPolicy::QueryAware(metadata) => metadata.authority_admitted && metadata.freshness_valid,
    };
    let recovery = recovery.filter(|r| {
        policy_admitted
            && super::delivery::can_resolve(r.store, r.scope, Some(r.resolver_token))
                .unwrap_or(false)
    });
    let full = measured_packet_content(packet.clone(), &basis)?;
    let full_tokens = measure_packet(&full, &basis)?;
    let mut reduced = packet.clone();
    let mut floor = packet.clone();
    if let Some(owner) = recovery {
        for (reduced_block, floor_block) in reduced.blocks.iter_mut().zip(floor.blocks.iter_mut()) {
            if reduced_block.protected {
                continue;
            }
            let original = reduced_block.text.clone();
            // Complete no-op/unsupported results stay exact; no second lossy
            // transform runs after a refusal or failed source-span proof.
            let path = Path::new(&reduced_block.source_ref);
            let (candidate, mappings) = if is_code_ext(path) {
                super::ast::render(path, &original)
            } else if is_structured_text(path, &original) {
                (original.clone(), Vec::new())
            } else {
                match super::fidelity::extract_lines(&original, original.len() / 2, &[]) {
                    Ok(result) => result,
                    Err(_) => (original.clone(), Vec::new()),
                }
            };
            if candidate == original || candidate.len() >= original.len() {
                continue;
            }
            if super::fidelity::validate(
                original.as_bytes(),
                &super::recovery::digest(original.as_bytes()),
                candidate.as_bytes(),
                &mappings,
                &[],
            )
            .is_err()
            {
                continue;
            }
            let reference = match owner.store.publish(
                owner.scope,
                original.as_bytes(),
                7 * 24 * 60 * 60 * 1000,
                super::recovery::now_ms(),
            ) {
                Ok(reference) => reference,
                Err(_) => continue,
            };
            // Keep protected lines even in floor. Error/constraint information
            // is not replaced by a pointer just because the original survives.
            let protected_spans = super::fidelity::protected_lines(&original);
            let protected_size = protected_spans.iter().map(|s| s.end - s.start).sum();
            let floor_text = if is_code_ext(path) {
                candidate.clone()
            } else {
                super::fidelity::extract_lines(&original, protected_size, &[])
                    .map(|x| x.0)
                    .unwrap_or_else(|_| original.clone())
            };
            let marker = format!(
                "\n[Push original: {}; expiresAt={}; store={}; resolve using membrane_push_resolve]",
                reference.handle, reference.expires_at, reference.store_id
            );
            reduced_block.text = format!("{candidate}{marker}");
            reduced_block.resolver = reference.handle.clone();
            reduced_block.recoverable = true;
            floor_block.text = format!("{floor_text}{marker}");
            floor_block.resolver = reference.handle;
            floor_block.recoverable = true;
        }
    }
    let mut reduced = measured_packet_content(reduced, &basis)?;
    let mut floor = measured_packet_content(floor, &basis)?;
    let mut reduced_tokens = measure_packet(&reduced, &basis)?;
    if reduced_tokens >= full_tokens {
        reduced = full.clone();
        reduced_tokens = full_tokens;
    }
    let mut floor_tokens = measure_packet(&floor, &basis)?;
    if floor_tokens >= reduced_tokens {
        floor = reduced.clone();
        floor_tokens = reduced_tokens;
    }
    let minimum = floor_tokens;
    let mut representations = Vec::new();
    for (id, tokens, content) in [
        ("full", full_tokens, full),
        ("reduced_1", reduced_tokens, reduced),
        ("floor", floor_tokens, floor),
    ] {
        let mut resolver_paths = Vec::new();
        for resolver in content["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block["resolver"].as_str())
        {
            if !resolver_paths.iter().any(|existing| existing == resolver) {
                resolver_paths.push(resolver.to_owned());
            }
        }
        representations.push(PacketReductionRepresentationV1 {id:id.into(), tokens,
            parent_ref:format!("packet://{}", packet.trace_id), protected:protected.clone(),
            evidence_refs:all_ids.clone(), resolver_paths, minimum_viable_tokens:minimum,
            coverage_note:"Exact serialized packet measurement; complete evidence or authorized retained original; host framing measured separately".into(), content});
    }
    telemetry::record(
        "packet-measure",
        full_tokens as usize,
        reduced_tokens as usize,
        Some("unit=tokens;basis=o200k_base/1;scope=serialized_packet"),
        None,
    );
    let plan = PacketReductionPlanV1 {
        schema_version: PacketReductionPlanV1::SCHEMA_VERSION,
        estimator_basis: basis,
        representations,
        protected,
        minimum_viable_tokens: minimum,
    };
    plan.validate()
        .map_err(PacketReductionRequestError::InvalidPlan)?;
    Ok(plan)
}
fn measured_packet_content(
    mut packet: ContextPacketV1,
    basis: &rightkit_context_protocol::host_observation::EstimatorBasisV1,
) -> Result<Value, PacketReductionRequestError> {
    // Validate the counter before publishing any representation.
    measure_packet(&Value::Null, basis)?;
    let mut content_tokens = 0usize;
    for block in &mut packet.blocks {
        let measured = rightkit_memory_core::ContextTokenAccounting::count_exact(&block.text)
            .map_err(PacketReductionRequestError::ContentSerialization)?;
        block.selected_tokens = Some(measured);
        block.rendered_tokens = Some(measured);
        content_tokens = content_tokens.saturating_add(measured);
    }
    packet.budget.admitted_tokens = content_tokens;
    let encoded = serde_json::to_value(packet)
        .map_err(|error| PacketReductionRequestError::ContentSerialization(error.to_string()))?;
    let mut packet: rightkit_context_protocol::ContextPacketV1 = serde_json::from_value(encoded)
        .map_err(|error| PacketReductionRequestError::ContentSerialization(error.to_string()))?;
    packet.provider_accounting.clear();
    for block in &mut packet.blocks {
        let (class, mode, lane, selected, rendered, chars) = if !block.text.is_empty() {
            (
                rightkit_context_protocol::DeliveryClass::Rendered,
                Some(rightkit_context_protocol::DeliveryMode::Inline),
                rightkit_context_protocol::BudgetLaneKind::Rendered,
                block.selected_tokens.unwrap_or(0),
                block.rendered_tokens.unwrap_or(0),
                u32::try_from(block.text.chars().count()).map_err(|error| {
                    PacketReductionRequestError::ContentSerialization(error.to_string())
                })?,
            )
        } else if !block.resolver.is_empty() {
            (
                rightkit_context_protocol::DeliveryClass::ResolverBacked,
                Some(rightkit_context_protocol::DeliveryMode::Reference),
                rightkit_context_protocol::BudgetLaneKind::ResolverBacked,
                0,
                0,
                0,
            )
        } else {
            (
                rightkit_context_protocol::DeliveryClass::MetadataOnly,
                None,
                rightkit_context_protocol::BudgetLaneKind::MetadataOnly,
                0,
                0,
                0,
            )
        };
        block.delivery_stage = Some(rightkit_context_protocol::DeliveryStage::Finalized);
        block.delivery_class = Some(class);
        block.delivery_mode = mode;
        block.lane = Some(lane);
        block.selected_tokens = Some(selected);
        block.rendered_tokens = Some(rendered);
        block.delivered_chars = Some(chars);
        let reason = block
            .drop_reason
            .unwrap_or(rightkit_context_protocol::DropReason::None);
        let accounting = packet
            .provider_accounting
            .entry(block.provider.clone())
            .or_insert(rightkit_context_protocol::ProviderAccountingV1 {
                delivery_stage: rightkit_context_protocol::DeliveryStage::Finalized,
                selected_tokens: 0,
                rendered_tokens: 0,
                delivered_chars: 0,
                drop_reason: reason,
            });
        accounting.selected_tokens = accounting.selected_tokens.saturating_add(selected);
        accounting.rendered_tokens = accounting.rendered_tokens.saturating_add(rendered);
        accounting.delivered_chars = accounting.delivered_chars.saturating_add(chars);
        if accounting.drop_reason != reason {
            accounting.drop_reason = rightkit_context_protocol::DropReason::Multiple;
        }
    }
    let reconciliation = rightkit_context_core::reconcile::reconcile(&packet);
    packet.reconciliation = Some(
        serde_json::from_value(serde_json::to_value(reconciliation).map_err(|error| {
            PacketReductionRequestError::ContentSerialization(error.to_string())
        })?)
        .map_err(|error| PacketReductionRequestError::ContentSerialization(error.to_string()))?,
    );
    serde_json::to_value(packet)
        .map_err(|error| PacketReductionRequestError::ContentSerialization(error.to_string()))
}

pub(crate) fn finalize_receipts(
    fields: &mut serde_json::Map<String, Value>,
    selected_packet: &Value,
) {
    const COPIED: &[&str] = &[
        "deliveryStage",
        "deliveryClass",
        "selectedTokens",
        "allottedTokens",
        "renderedTokens",
        "deliveredChars",
        "dropReason",
    ];
    let delivered: std::collections::BTreeMap<&str, &Value> = selected_packet
        .get("blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            block
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id, block))
        })
        .collect();
    let Some(receipts) = fields.get_mut("receipts").and_then(Value::as_array_mut) else {
        return;
    };
    for receipt in receipts.iter_mut().filter_map(Value::as_object_mut) {
        let Some(id) = receipt.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        if let Some(block) = delivered.get(id.as_str()) {
            for key in COPIED {
                if let Some(value) = block.get(*key) {
                    receipt.insert((*key).to_owned(), value.clone());
                }
            }
        } else {
            receipt.insert(
                "deliveryStage".to_owned(),
                Value::String("finalized".to_owned()),
            );
            receipt.insert(
                "deliveryClass".to_owned(),
                Value::String("metadata_only".to_owned()),
            );
            receipt.insert("selectedTokens".to_owned(), Value::from(0u64));
            receipt.remove("allottedTokens");
            receipt.insert("renderedTokens".to_owned(), Value::from(0u64));
            receipt.insert("deliveredChars".to_owned(), Value::from(0u64));
            let preserved = receipt
                .get("dropReason")
                .and_then(Value::as_str)
                .is_some_and(|reason| reason != "none");
            if !preserved {
                receipt.insert(
                    "dropReason".to_owned(),
                    Value::String("not_selected".to_owned()),
                );
            }
        }
    }
}

pub fn select_packet_for_h8(
    packet: &ContextPacketV1,
    ceiling: &RemainingContextCeilingV1,
) -> Result<PacketReductionSelectionV1, PacketReductionRequestError> {
    select_packet_for_h8_with_policy(packet, ceiling, &PushPolicy::Control)
}
pub fn select_packet_for_h8_with_policy(
    packet: &ContextPacketV1,
    ceiling: &RemainingContextCeilingV1,
    policy: &PushPolicy,
) -> Result<PacketReductionSelectionV1, PacketReductionRequestError> {
    select_packet_for_h8_with_recovery(packet, ceiling, policy, None)
}
pub fn select_packet_for_h8_with_recovery(
    packet: &ContextPacketV1,
    ceiling: &RemainingContextCeilingV1,
    policy: &PushPolicy,
    recovery: Option<&RecoveryContext<'_>>,
) -> Result<PacketReductionSelectionV1, PacketReductionRequestError> {
    let plan = build_with_recovery(
        packet,
        ceiling.remaining_tokens.basis.clone(),
        policy,
        recovery,
    )?;
    let selected = plan
        .select_for_capacity(ceiling)
        .map_err(PacketReductionRequestError::Selection)?
        .clone();
    let remaining_tokens = ceiling.remaining_tokens.estimate.value.ok_or({
        PacketReductionRequestError::H8(RequestTimeH8Error::Inexact {
            coverage: ceiling.remaining_tokens.estimate.coverage,
            reason: ceiling.remaining_tokens.estimate.unavailable_reason,
        })
    })?;
    let receipt = PacketReductionSelectionReceiptV1 {
        schema_version: PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION,
        plan_ref: selected.parent_ref.clone(),
        ceiling_id: ceiling.ceiling_id.clone(),
        session_id: ceiling.session_id.clone(),
        selected_representation_id: selected.id.clone(),
        selected_tokens: selected.tokens,
        remaining_tokens,
        estimator_basis: plan.estimator_basis.clone(),
        decision: "selected".into(),
    };
    Ok(PacketReductionSelectionV1 {
        plan,
        selected_representation: selected,
        selection_receipt: receipt,
    })
}

/// Bounded-response selection (implementation-contract §"Pull budget
/// contract", PUL-050/051): the same measured full/reduced/floor ladder and
/// protected-item invariants as `select_packet_for_h8_with_recovery`, but the
/// capacity bound is the caller's declared response budget in
/// `o200k_base/1` tokens rather than a host observation. No host identity is
/// invented — the receipt names the declared budget instead of a ceiling
/// identity. A budget below the minimum viable representation is the typed
/// `NoRepresentationFits` refusal: never a partial, truncated, or
/// protected-evidence-dropping emission.
pub fn select_packet_for_token_budget(
    packet: &ContextPacketV1,
    budget_tokens: u64,
    policy: &PushPolicy,
    recovery: Option<&RecoveryContext<'_>>,
) -> Result<PacketReductionSelectionV1, PacketReductionRequestError> {
    if budget_tokens == 0 {
        return Err(PacketReductionRequestError::Selection(
            PacketReductionSelectionError::NoRepresentationFits {
                remaining_tokens: 0,
                minimum_viable_tokens: u64::MAX,
            },
        ));
    }
    let basis =
        rightkit_context_protocol::host_observation::EstimatorBasisV1::new("o200k_base", "1");
    let plan = build_with_recovery(packet, basis, policy, recovery)?;
    let mut selected: Option<&PacketReductionRepresentationV1> = None;
    for representation in &plan.representations {
        if representation.tokens <= budget_tokens
            && selected
                .as_ref()
                .is_none_or(|current| representation.tokens > current.tokens)
        {
            selected = Some(representation);
        }
    }
    let selected = selected
        .ok_or(PacketReductionSelectionError::NoRepresentationFits {
            remaining_tokens: budget_tokens,
            minimum_viable_tokens: plan.minimum_viable_tokens,
        })
        .map_err(PacketReductionRequestError::Selection)?
        .clone();
    let receipt = PacketReductionSelectionReceiptV1 {
        schema_version: PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION,
        plan_ref: selected.parent_ref.clone(),
        ceiling_id: format!("budget://declared/{budget_tokens}"),
        session_id: packet.trace_id.clone(),
        selected_representation_id: selected.id.clone(),
        selected_tokens: selected.tokens,
        remaining_tokens: budget_tokens.saturating_sub(selected.tokens),
        estimator_basis: plan.estimator_basis.clone(),
        decision: "selected".into(),
    };
    Ok(PacketReductionSelectionV1 {
        plan,
        selected_representation: selected,
        selection_receipt: receipt,
    })
}

/// The mapping sentence `deliveredResponse` carries: what its numbers count.
const WHOLE_RESPONSE_MAPPING: &str = "o200k_base/1 count and byte length of the exact bytes delivered in this mode (json: the whole document; text: the context and a newline); packetTokens: the serialized packet, not delivered";

/// The most passes, after the seed projection, the self-referential accounting gets to settle.
pub const WHOLE_RESPONSE_MAX_PASSES: usize = 4;

const PROJECTION_UNSTABLE_REASON: &str =
    "request_time_selection_refused:budget_insufficient:projection unstable";

/// `detail.deliveredResponse` for an explicit pull: the accounting of the complete response its
/// consumer receives in `mode` (`json` or `text`). `whole_tokens` and `whole_bytes` count the
/// exact projected bytes, `context_tokens` the context text inside them, `packet_tokens` the
/// serialized packet (never delivered); every count is o200k_base/1.
pub fn whole_response_accounting(
    mode: &str,
    whole_tokens: u64,
    whole_bytes: u64,
    context_tokens: u64,
    packet_tokens: u64,
    fits_budget: bool,
) -> Value {
    serde_json::json!({
        "schemaVersion": 1,
        "mode": mode,
        "wholeResponseTokens": whole_tokens,
        "wholeResponseBytes": whole_bytes,
        "contextTokens": context_tokens,
        "packetTokens": packet_tokens,
        "unit": "tokens",
        "estimatorBasis": "o200k_base/1",
        "scope": "whole_response",
        "selectedBy": "whole_response",
        "fitsBudget": fits_budget,
        "mapping": WHOLE_RESPONSE_MAPPING,
    })
}

/// Why a whole-response selection delivered nothing.
#[derive(Debug)]
pub enum WholeResponseError {
    /// Complete evidence exists but no representation's complete response fits the declared
    /// budget. A smaller packet allowance may still admit a smaller one. Carries the full
    /// `request_time_selection_refused:budget_insufficient:...` reason.
    Capacity(String),
    /// Refused outright (unstable projection, tokenizer, deadline); refitting cannot help.
    Fatal(String),
    /// The reduction plan could not be built.
    Plan(PacketReductionRequestError),
}

/// A selected representation with the accounting of its complete response.
pub struct WholeResponseSelection {
    pub selection: PacketReductionSelectionV1,
    /// The `deliveredResponse` value whose projection was counted and fits the budget.
    pub delivered_response: Value,
}

/// Settle the self-referential accounting of one representation: project the complete response
/// with the accounting present, count it, and feed the count back until the projection of the
/// accounting equals the accounting it contains. One seed projection (accounting zeroed) fixes
/// the digit widths; then at most [`WHOLE_RESPONSE_MAX_PASSES`] passes, each inside the request
/// deadline (`live`). Every other value is frozen (`fitsBudget` is held true while settling; it
/// is true for any response that is delivered). A projection that does not settle is a typed
/// refusal, never a guess. Returns the settled tokens and the accounting.
fn settle_whole_response(
    content: &Value,
    mode: &str,
    context_tokens: u64,
    packet_tokens: u64,
    live: &dyn Fn() -> Result<(), String>,
    project: &dyn Fn(&Value, &Value) -> Result<String, String>,
) -> Result<(u64, Value), WholeResponseError> {
    let pass = |tokens: u64, bytes: u64| -> Result<(u64, u64, Value), WholeResponseError> {
        live().map_err(WholeResponseError::Fatal)?;
        let accounting =
            whole_response_accounting(mode, tokens, bytes, context_tokens, packet_tokens, true);
        let projected = project(content, &accounting).map_err(WholeResponseError::Fatal)?;
        let counted = crate::hook::count_o200k(&projected).map_err(WholeResponseError::Fatal)?;
        Ok((counted, projected.len() as u64, accounting))
    };
    let (mut tokens, mut bytes, _) = pass(0, 0)?;
    for _ in 0..WHOLE_RESPONSE_MAX_PASSES {
        let (counted, measured, accounting) = pass(tokens, bytes)?;
        if counted == tokens && measured == bytes {
            return Ok((tokens, accounting));
        }
        tokens = counted;
        bytes = measured;
    }
    Err(WholeResponseError::Fatal(
        PROJECTION_UNSTABLE_REASON.to_owned(),
    ))
}

/// Explicit-pull selection under a declared response budget (PUL-050/051): the budget bounds the
/// complete response the consumer receives in `mode`, not the block text inside it.
///
/// Same measured full/reduced/floor ladder and protected-item invariants as
/// [`select_packet_for_token_budget`]. For each representation `project` builds the exact bytes
/// the consumer receives (from the representation's content and a candidate `deliveredResponse`),
/// they are counted exactly under o200k_base/1 and the accounting is settled to a fixpoint
/// (`settle_whole_response`). The representation with the largest complete response that fits
/// `budget_tokens` is selected; the serialized packet measurement stays on the representation
/// and is reported beside it as `packetTokens`. The selection receipt's `selectedTokens` is the
/// complete-response count (`decision` says so). A budget below every representation's complete
/// response is the typed `budget_insufficient` refusal naming the largest and smallest sizes.
/// Nothing is written while selecting.
pub fn select_packet_for_whole_response(
    packet: &ContextPacketV1,
    budget_tokens: u64,
    mode: &str,
    policy: &PushPolicy,
    recovery: Option<&RecoveryContext<'_>>,
    live: &dyn Fn() -> Result<(), String>,
    project: &dyn Fn(&Value, &Value) -> Result<String, String>,
) -> Result<WholeResponseSelection, WholeResponseError> {
    if budget_tokens == 0 {
        return Err(WholeResponseError::Capacity(
            "request_time_selection_refused:budget_insufficient:declared budget is 0".to_owned(),
        ));
    }
    let basis =
        rightkit_context_protocol::host_observation::EstimatorBasisV1::new("o200k_base", "1");
    let plan =
        build_with_recovery(packet, basis, policy, recovery).map_err(WholeResponseError::Plan)?;
    let mut selected: Option<(&PacketReductionRepresentationV1, u64, Value)> = None;
    let mut largest = 0u64;
    let mut smallest = u64::MAX;
    for representation in &plan.representations {
        // A representation that delivers no text delivers nothing: never a fit.
        let context = consumer_visible_text(&representation.content).unwrap_or_default();
        if context.trim().is_empty() {
            continue;
        }
        let context_tokens =
            crate::hook::count_o200k(&context).map_err(WholeResponseError::Fatal)?;
        let (tokens, accounting) = settle_whole_response(
            &representation.content,
            mode,
            context_tokens,
            representation.tokens,
            live,
            project,
        )?;
        largest = largest.max(tokens);
        smallest = smallest.min(tokens);
        if tokens <= budget_tokens
            && selected
                .as_ref()
                .is_none_or(|(_, current, _)| tokens > *current)
        {
            selected = Some((representation, tokens, accounting));
        }
    }
    let Some((selected, whole_tokens, accounting)) = selected else {
        return Err(WholeResponseError::Capacity(format!(
            "request_time_selection_refused:budget_insufficient:complete {mode} response {largest} tokens exceeds declared {budget_tokens} (smallest {})",
            if smallest == u64::MAX { 0 } else { smallest }
        )));
    };
    let selected = selected.clone();
    let receipt = PacketReductionSelectionReceiptV1 {
        schema_version: PACKET_REDUCTION_SELECTION_RECEIPT_SCHEMA_VERSION,
        plan_ref: selected.parent_ref.clone(),
        ceiling_id: format!("budget://declared/{budget_tokens}"),
        session_id: packet.trace_id.clone(),
        selected_representation_id: selected.id.clone(),
        selected_tokens: whole_tokens,
        remaining_tokens: budget_tokens.saturating_sub(whole_tokens),
        estimator_basis: plan.estimator_basis.clone(),
        decision: "selected_by_whole_response".into(),
    };
    Ok(WholeResponseSelection {
        selection: PacketReductionSelectionV1 {
            plan,
            selected_representation: selected,
            selection_receipt: receipt,
        },
        delivered_response: accounting,
    })
}
