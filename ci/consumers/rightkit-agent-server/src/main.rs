// rightkit-agent-server 0.1.0 consumed by exact registry pin: the host entry point is linkable.
use std::sync::Arc;

use rightkit_agent_host_traits::{skill_host::SkillHost, SkillCatalog};

fn main() {
    let entry: fn(Arc<dyn SkillCatalog>, Arc<dyn SkillHost>) = rightkit_agent_server::run;
    if (entry as usize) == 0 {
        eprintln!("FAIL: run entry point missing");
        std::process::exit(1);
    }
    println!("ok: rightkit_agent_server::run is linkable");
}
