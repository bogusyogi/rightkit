mod tests {
    use super::super::*;
    use std::collections::HashMap;

    fn stdio(command: &str, args: &[&str]) -> StdioConfig {
        StdioConfig {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: HashMap::new(),
        }
    }

    #[test]
    fn fresh_workspace_server_is_pending() {
        let store = TrustStore::new();
        let cfg = stdio("npx", &["-y", "@evil/server"]);
        assert_eq!(
            decide_workspace_stdio(&store, "/ws/one", "evil", &cfg),
            TrustDecision::Pending
        );
    }

    #[test]
    fn approve_then_same_content_spawns_and_changed_content_reprompts() {
        let mut store = TrustStore::new();
        let cfg = stdio("npx", &["-y", "@ok/server"]);
        store.approve("/ws/one", "ok", &stdio_server_spec(&cfg));
        assert_eq!(
            decide_workspace_stdio(&store, "/ws/one", "ok", &cfg),
            TrustDecision::Approved
        );
        let mutated = stdio("npx", &["-y", "@ok/server", "--extra-flag"]);
        assert_eq!(
            decide_workspace_stdio(&store, "/ws/one", "ok", &mutated),
            TrustDecision::Pending
        );
    }

    #[test]
    fn deny_then_same_content_never_spawns() {
        let mut store = TrustStore::new();
        let cfg = stdio("npx", &["-y", "@bad/server"]);
        store.deny("/ws/one", "bad", &stdio_server_spec(&cfg));
        assert_eq!(
            decide_workspace_stdio(&store, "/ws/one", "bad", &cfg),
            TrustDecision::Denied
        );
    }

    #[test]
    fn pending_approval_never_leaks_env_values() {
        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_supersecret".to_string());
        let cfg = StdioConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "server".to_string()],
            env,
        };
        let store = TrustStore::new();
        let pending = store.pending_approval("/ws", "gh", &stdio_server_spec(&cfg));
        assert_eq!(pending.env_keys, vec!["GITHUB_TOKEN".to_string()]);
        assert!(!serde_json::to_string(&pending)
            .unwrap()
            .contains("ghp_supersecret"));
    }

    #[test]
    fn gate_splits_approved_denied_and_pending() {
        let mut store = TrustStore::new();
        let approved_cfg = stdio("npx", &["-y", "@approved/server"]);
        store.approve("/ws", "approved", &stdio_server_spec(&approved_cfg));
        let denied_cfg = stdio("npx", &["-y", "@denied/server"]);
        store.deny("/ws", "denied", &stdio_server_spec(&denied_cfg));

        let mut servers = HashMap::new();
        servers.insert("approved".to_string(), approved_cfg);
        servers.insert("denied".to_string(), denied_cfg);
        servers.insert("fresh".to_string(), stdio("npx", &["-y", "@fresh/server"]));

        let (approved, pending) = gate_workspace_stdio_servers(&store, "/ws", &servers);
        assert_eq!(approved, vec!["approved".to_string()]);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].server, "fresh");
    }

    #[test]
    fn load_missing_file_yields_empty_store_and_corrupt_file_stays_gated() {
        let dir = std::env::temp_dir().join(format!("rightkit-agent-mcp-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = stdio("npx", &["x"]);
        let missing = dir.join("missing.json");
        assert_eq!(
            decide_workspace_stdio(&load_trust_store(&missing), "/ws", "x", &cfg),
            TrustDecision::Pending
        );
        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, "not json").unwrap();
        assert_eq!(
            decide_workspace_stdio(&load_trust_store(&corrupt), "/ws", "x", &cfg),
            TrustDecision::Pending
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
