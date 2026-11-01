mod tests {
    use super::super::*;
    use crate::agent_mcp::types::{HttpConfig, McpChannelPermissions, SseConfig, StdioConfig};

    fn env_vars() -> HashMap<String, String> {
        HashMap::from([
            ("MCP_TOKEN".to_string(), "secret-token".to_string()),
            ("MCP_HOST".to_string(), "127.0.0.1".to_string()),
        ])
    }

    #[test]
    fn expands_stdio_env_placeholders() {
        let mut config = McpConfigFile {
            mcp_servers: HashMap::from([(
                "server".to_string(),
                McpServerConfig::Stdio(StdioConfig {
                    command: "node".to_string(),
                    args: vec![],
                    env: HashMap::from([
                        ("API_KEY".to_string(), "${MCP_TOKEN}".to_string()),
                        (
                            "BASE_URL".to_string(),
                            "http://${MCP_HOST}:9100/${MCP_TOKEN}".to_string(),
                        ),
                    ]),
                }),
            )]),
            mcp_trust: HashMap::new(),
            mcp_channel_permissions: HashMap::new(),
            mcp_oauth: HashMap::new(),
            mcp_oauth_tokens: HashMap::new(),
        };

        expand_config_env(&mut config, &env_vars());

        let McpServerConfig::Stdio(stdio) = &config.mcp_servers["server"] else {
            panic!("expected stdio config");
        };
        assert_eq!(stdio.env["API_KEY"], "secret-token");
        assert_eq!(stdio.env["BASE_URL"], "http://127.0.0.1:9100/secret-token");
    }

    #[test]
    fn leaves_missing_or_invalid_placeholders_visible() {
        let vars = env_vars();

        assert_eq!(expand_env_placeholders("${MISSING}", &vars), "${MISSING}");
        assert_eq!(
            expand_env_placeholders("before ${} after", &vars),
            "before ${} after"
        );
        assert_eq!(
            expand_env_placeholders("before ${MCP-TOKEN} after", &vars),
            "before ${MCP-TOKEN} after"
        );
        assert_eq!(
            expand_env_placeholders("before ${MCP_TOKEN", &vars),
            "before ${MCP_TOKEN"
        );
    }

    #[test]
    fn never_expands_a_licence_or_device_variable() {
        let mut vars = env_vars();
        for name in [
            "RIGHTKIT_SIGNED_LICENSE_TOKEN",
            "RIGHTKIT_LICENSE_DEVICE_ID",
            "RIGHTKIT_LICENSE_DIR",
        ] {
            vars.insert(name.to_string(), "must-not-leak".to_string());
            let placeholder = format!("${{{name}}}");
            assert_eq!(expand_env_placeholders(&placeholder, &vars), placeholder);
        }
    }

    #[test]
    fn does_not_modify_non_stdio_configs() {
        let mut config = McpConfigFile {
            mcp_servers: HashMap::from([(
                "http".to_string(),
                McpServerConfig::Http(HttpConfig {
                    url: "http://${MCP_HOST}:9100/mcp".to_string(),
                }),
            )]),
            mcp_trust: HashMap::new(),
            mcp_channel_permissions: HashMap::new(),
            mcp_oauth: HashMap::new(),
            mcp_oauth_tokens: HashMap::new(),
        };

        expand_config_env(&mut config, &env_vars());

        let McpServerConfig::Http(http) = &config.mcp_servers["http"] else {
            panic!("expected http config");
        };
        assert_eq!(http.url, "http://${MCP_HOST}:9100/mcp");
    }

    #[test]
    fn build_manager_registers_channel_permissions_for_servers() {
        let manager = build_manager(McpConfigFile {
            mcp_servers: HashMap::from([(
                "docs".to_string(),
                McpServerConfig::Sse(SseConfig {
                    url: "http://127.0.0.1:9100/sse".to_string(),
                    allow_cross_origin_endpoint: false,
                }),
            )]),
            mcp_trust: HashMap::new(),
            mcp_channel_permissions: HashMap::from([(
                "docs".to_string(),
                McpChannelPermissions {
                    allowed_channels: vec!["desktop".to_string()],
                    allow_mutating: false,
                },
            )]),
            mcp_oauth: HashMap::new(),
            mcp_oauth_tokens: HashMap::new(),
        });

        assert!(manager.is_channel_allowed("docs", "desktop"));
        assert!(!manager.is_channel_allowed("docs", "telegram"));
    }

    #[test]
    fn build_manager_registers_trust_policy_for_servers() {
        let manager = build_manager(McpConfigFile {
            mcp_servers: HashMap::from([(
                "docs".to_string(),
                McpServerConfig::Sse(SseConfig {
                    url: "http://127.0.0.1:9100/sse".to_string(),
                    allow_cross_origin_endpoint: false,
                }),
            )]),
            mcp_trust: HashMap::from([(
                "docs".to_string(),
                crate::agent_mcp::types::McpTrustPolicy {
                    preset: "untrusted".to_string(),
                    approved_by: None,
                    approved_at: None,
                },
            )]),
            mcp_channel_permissions: HashMap::new(),
            mcp_oauth: HashMap::new(),
            mcp_oauth_tokens: HashMap::new(),
        });

        assert_eq!(manager.trust_policy("docs").unwrap().preset, "untrusted");
    }

    #[test]
    fn config_file_parses_http_oauth_metadata() {
        let json = r#"{
          "mcpServers": {
            "remote": {
              "type": "http",
              "url": "https://mcp.example.com/mcp",
              "oauth": {
                "authorizationUrl": "https://auth.example.com/authorize",
                "tokenUrl": "https://auth.example.com/token",
                "revocationUrl": "https://auth.example.com/revoke",
                "clientId": "rightkit-agent",
                "scopes": ["mcp:tools"],
                "resource": "https://mcp.example.com/mcp"
              }
            }
          }
        }"#;

        let cfg: McpConfigFile = serde_json::from_str(json).unwrap();

        let oauth = cfg.mcp_oauth.get("remote").expect("oauth config");
        assert_eq!(oauth.client_id, "rightkit-agent");
        assert_eq!(oauth.resource, "https://mcp.example.com/mcp");
        assert_eq!(oauth.scopes, vec!["mcp:tools"]);
        assert_eq!(
            oauth.revocation_url.as_deref(),
            Some("https://auth.example.com/revoke")
        );
    }

    #[test]
    fn inject_stored_oauth_tokens_adds_bearer_to_http_and_sse() {
        let mut config = McpConfigFile {
            mcp_servers: HashMap::from([
                (
                    "http".to_string(),
                    McpServerConfig::Http(HttpConfig {
                        url: "https://mcp.example.com/mcp".to_string(),
                    }),
                ),
                (
                    "sse".to_string(),
                    McpServerConfig::Sse(SseConfig {
                        url: "https://mcp.example.com/sse".to_string(),
                        allow_cross_origin_endpoint: false,
                    }),
                ),
            ]),
            mcp_trust: HashMap::new(),
            mcp_channel_permissions: HashMap::new(),
            mcp_oauth: HashMap::new(),
            mcp_oauth_tokens: HashMap::new(),
        };
        let tokens = HashMap::from([
            (
                "http".to_string(),
                crate::agent_mcp::oauth::McpOAuthTokens {
                    access_token: "http-token".to_string(),
                    refresh_token: None,
                    token_type: "Bearer".to_string(),
                    expires_at_unix: None,
                    scope: None,
                    resource: Some("https://mcp.example.com/mcp".to_string()),
                    token_url: None,
                },
            ),
            (
                "sse".to_string(),
                crate::agent_mcp::oauth::McpOAuthTokens {
                    access_token: "sse-token".to_string(),
                    refresh_token: None,
                    token_type: "Bearer".to_string(),
                    expires_at_unix: None,
                    scope: None,
                    resource: Some("https://mcp.example.com/sse".to_string()),
                    token_url: None,
                },
            ),
        ]);

        attach_stored_oauth_tokens(&mut config, &tokens);

        assert_eq!(
            config
                .mcp_oauth_tokens
                .get("http")
                .map(|tokens| tokens.access_token.as_str()),
            Some("http-token")
        );
        assert_eq!(
            config
                .mcp_oauth_tokens
                .get("sse")
                .map(|tokens| tokens.access_token.as_str()),
            Some("sse-token")
        );
    }

    #[test]
    fn config_sources_include_compatibility_and_host_local_files() {
        let root = std::env::temp_dir().join(format!(
            "rightkit-agent-mcp-config-sources-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = config_paths(Some(&root));
        let compatibility = root.join(".mcp.json");
        let local = root.join(".rightkit/mcp.json");

        let compatibility_index = paths
            .iter()
            .position(|path| path == &compatibility)
            .unwrap();
        let local_index = paths.iter().position(|path| path == &local).unwrap();
        assert!(local_index > compatibility_index);
    }
}
