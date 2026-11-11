// rightkit-agent-task-contract 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only: the task-state table, the effect classes, the approval binding's
// wire form on the approvals core's Binding, and the voice hand-off with caller-supplied labels.
// No I/O, no network, no process spawn. Exits 1 on the first failed check.
use rightkit_agent_task_contract::task::{is_legal_transition, transition, TaskStatus};
use rightkit_agent_task_contract::{
    ApprovalBinding, Binding, EffectClass, VoiceHandoff, VoiceLabels,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn binding_json(args_hash: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "principal": {"schema_version": 2, "kind": "owner", "subject": "owner", "connection": null},
        "task_id": "task-1",
        "step_id": null,
        "effect": "publication",
        "args_hash": args_hash,
        "destination": "git@example.invalid:repo.git",
        "executor": {
            "schema_version": 1,
            "device": {"schema_version": 1, "device_id": "dev_1", "platform": "mac", "display_name": null},
            "executor": "shell"
        },
        "scope": ["push"],
        "expires_at_ms": 2000,
        "revision": 3
    })
}

fn handoff(r1: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "utterance": {"schema_version": 1, "utterance_id": "utt-1", "generation": 2},
        "session_id": "session-1",
        "transcript": "what is on my calendar",
        "confidence": 0.9,
        "device": {"schema_version": 1, "device_id": "dev_1", "platform": "mac", "display_name": null},
        "owner_id": "owner",
        "mode": "addressed-control",
        "checks": {
            "schema_version": 1,
            "r0": {"outcome": "no-exact-match", "catalog_version": "1"},
            "r1": {"outcome": r1, "registry_version": "1"}
        }
    })
}

fn main() {
    check(
        transition(TaskStatus::Accepted, TaskStatus::Leased) == Ok(TaskStatus::Leased),
        "accepted to leased is a legal transition",
    );
    check(
        is_legal_transition(TaskStatus::Running, TaskStatus::Uncertain)
            && !is_legal_transition(TaskStatus::Running, TaskStatus::WaitingDevice),
        "a running task that loses its outcome goes to uncertain, never back to waiting-device",
    );
    let refused = transition(TaskStatus::Succeeded, TaskStatus::Running);
    check(
        matches!(refused, Err(e) if e.from == TaskStatus::Succeeded && e.to == TaskStatus::Running),
        "succeeded to running is refused with IllegalTransition",
    );
    check(
        !TaskStatus::Uncertain.is_terminal()
            && !TaskStatus::CancelRequested.is_terminal()
            && TaskStatus::Cancelled.is_terminal(),
        "uncertain and cancel-requested are not terminal; cancelled is",
    );
    check(
        !EffectClass::Reversible.is_irreversible() && EffectClass::Publication.is_irreversible(),
        "only reversible effects skip the owner approval check",
    );

    let wire = binding_json(&format!("sha256:{HEX}"));
    let record: ApprovalBinding = match serde_json::from_value(wire.clone()) {
        Ok(record) => record,
        Err(error) => {
            check(false, &format!("an approval binding reads: {error}"));
            unreachable!()
        }
    };
    check(
        record.core_binding()
            == &Binding {
                args: HEX.to_string(),
                destination: "git@example.invalid:repo.git".to_string(),
                expires_ms: 2000,
                revision: 3,
            },
        "args_hash, destination, expiry and revision read into the core binding",
    );
    check(
        serde_json::to_value(&record).ok() == Some(wire),
        "the record writes back to the same flat wire form",
    );
    check(
        serde_json::from_value::<ApprovalBinding>(binding_json(HEX)).is_err()
            && serde_json::from_value::<ApprovalBinding>(binding_json("sha256:ABC")).is_err(),
        "an args_hash that is not sha256: plus 64 lowercase hex is refused",
    );
    let mut changed = record.binding.clone();
    changed.args = "f".repeat(64);
    check(
        record.binding.check(&record.binding, 1000).is_ok()
            && record.binding.check(&changed, 1000).is_err()
            && record.binding.check(&record.binding, 3000).is_err(),
        "the core rule refuses changed arguments and a use past the expiry",
    );

    let labels = VoiceLabels {
        chat: "voice-app:control".to_string(),
        policy_ref: "voice-control/v1".to_string(),
    };
    let voice: VoiceHandoff = serde_json::from_value(handoff("unresolved-agent-request"))
        .unwrap_or_else(|error| {
            check(false, &format!("a voice hand-off reads: {error}"));
            unreachable!()
        });
    let envelope = voice.to_task_envelope(&labels);
    check(
        matches!(&envelope, Ok(e) if e.policy_ref == "voice-control/v1"
            && e.idempotency_key == "voice:utt-1:2"
            && serde_json::to_value(&e.intent).ok().and_then(|v| v["source"]["chat"].as_str().map(str::to_string))
                == Some("voice-app:control".to_string())),
        "a hand-off maps to an envelope with the caller's chat label and policy reference",
    );
    let empty = VoiceLabels {
        chat: String::new(),
        policy_ref: "voice-control/v1".to_string(),
    };
    check(
        voice.to_task_envelope(&empty).is_err(),
        "an empty chat label is refused",
    );
    let local: VoiceHandoff =
        serde_json::from_value(handoff("resolved-local")).unwrap_or_else(|error| {
            check(
                false,
                &format!("a locally resolved hand-off reads: {error}"),
            );
            unreachable!()
        });
    check(
        local.to_task_envelope(&labels).is_err(),
        "a hand-off R1 resolved locally is not submitted to the agent",
    );
}
