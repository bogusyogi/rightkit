// rightkit-agent-engine-runtime-resources 0.1.0 consumed by exact registry pin: profiles, governor, bounded output.
use rightkit_agent_engine_runtime_resources::{
    BoundedOutput, ResourceGovernor, ResourceProfile, RETAINED_BYTES,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        ResourceProfile::parse("strict") == Ok(ResourceProfile::Strict),
        "parse strict",
    );
    check(
        ResourceProfile::parse("nope").is_err(),
        "parse rejects unknown profiles",
    );
    let gov = ResourceGovernor::for_profile(ResourceProfile::Practical);
    check(
        gov.profile() == ResourceProfile::Practical,
        "governor keeps its profile",
    );
    let mut out = BoundedOutput::default();
    out.push(&vec![b'x'; RETAINED_BYTES + 10]);
    check(
        out.truncated() && out.total_bytes() == (RETAINED_BYTES + 10) as u64,
        "BoundedOutput truncates and counts",
    );
}
