mod core;
mod infrastructure;
mod specialists;
mod workers;

use crate::coding::registry::AgentDescriptor;

pub fn builtin_agents() -> Vec<AgentDescriptor> {
    let mut agents = Vec::new();
    agents.extend(core::core_agents());
    agents.extend(specialists::specialist_agents());
    agents.extend(workers::worker_agents());
    agents.extend(infrastructure::infrastructure_agents());
    agents
}
