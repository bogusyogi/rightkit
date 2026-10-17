//! Review epoch of cited facts and brief re-entry (#314).

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{parse, BriefReentry, ChannelMemory, ProofStatus};

const LAUNCH: &str = "proof.receipt-launch-week";

fn load() -> ChannelMemory {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts-pending/channel-memory/fixtures/valid")
        .join("proof-bank-rev2.json");
    let text = fs::read_to_string(path).expect("read fixture");
    parse(&text).expect("valid fixture")
}

#[test]
fn unchanged_facts_stay_current() {
    let memory = load();
    let ids = [LAUNCH];
    let epoch = memory.review_epoch(&ids);
    assert_eq!(memory.brief_reentry(&ids, &epoch), BriefReentry::Current);
}

#[test]
fn a_changed_fact_sends_the_brief_back_to_review() {
    let mut memory = load();
    let ids = [LAUNCH];
    let epoch = memory.review_epoch(&ids);
    let proof = memory
        .proofs
        .iter_mut()
        .find(|item| item.id == LAUNCH)
        .expect("proof in fixture");
    proof.status = ProofStatus::Withdrawn;
    match memory.brief_reentry(&ids, &epoch) {
        BriefReentry::NeedsReview { changed, .. } => assert_eq!(changed, vec![LAUNCH]),
        other => panic!("expected re-entry, got {other:?}"),
    }
}
