mod tests {
    use super::super::*;

    #[test]
    fn tool_definitions_have_correct_names() {
        let tools = lsp_mcp_tool_definitions();
        let names: Vec<_> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"lsp_goto_definition"));
        assert!(names.contains(&"lsp_hover"));
        assert!(names.contains(&"lsp_references"));
        assert!(names.contains(&"lsp_diagnostics"));
    }

    #[test]
    fn diagnostics_document_sync_notifications_open_and_save_current_text() {
        let notifications =
            diagnostics_document_sync_notifications("file:///tmp/a.rs", "rust", "fn main() {}\n");

        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0]["method"], "textDocument/didOpen");
        assert_eq!(
            notifications[0]["params"]["textDocument"]["languageId"],
            "rust"
        );
        assert_eq!(
            notifications[0]["params"]["textDocument"]["text"],
            "fn main() {}\n"
        );
        assert_eq!(notifications[1]["method"], "textDocument/didSave");
        assert_eq!(
            notifications[1]["params"]["textDocument"]["uri"],
            "file:///tmp/a.rs"
        );
        assert_eq!(notifications[1]["params"]["text"], "fn main() {}\n");
    }

    #[test]
    fn document_sync_opens_once_then_changes_with_monotonic_versions() {
        let (open, version) =
            document_sync_notification("file:///tmp/a.rs", "rust", "fn main() {}\n", None);
        assert_eq!(open["method"], "textDocument/didOpen");
        assert_eq!(open["params"]["textDocument"]["version"], 1);
        assert_eq!(version, 1);

        let (change, next_version) = document_sync_notification(
            "file:///tmp/a.rs",
            "rust",
            "fn main() { println!(\"ok\"); }\n",
            Some(version),
        );
        assert_eq!(change["method"], "textDocument/didChange");
        assert_eq!(change["params"]["textDocument"]["version"], 2);
        assert_eq!(
            change["params"]["contentChanges"][0]["text"],
            "fn main() { println!(\"ok\"); }\n"
        );
        assert_eq!(next_version, 2);
    }

    #[test]
    fn reference_locations_parse_like_definition_locations() {
        let result = serde_json::json!([{
            "uri": "file:///tmp/a.rs",
            "range": {
                "start": {"line": 4, "character": 2},
                "end": {"line": 4, "character": 9}
            }
        }]);
        let locations = extract_reference_locations(&result);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, "file:///tmp/a.rs");
        assert_eq!(locations[0].start.line, 4);
    }

    #[tokio::test]
    async fn bridge_requires_initialization() {
        let mut bridge = LspMcpBridge::new(LspServerConfig {
            language_id: "rust".into(),
            server_path: "/usr/bin/rust-analyzer".into(),
            server_args: vec![],
            workspace_root: "/tmp".into(),
        });
        let result = bridge
            .goto_definition(
                "file:///tmp/foo.rs",
                LspPosition {
                    line: 0,
                    character: 0,
                },
            )
            .await;
        assert!(matches!(result, Err(LspBridgeError::NotInitialized)));
    }

    #[tokio::test]
    async fn framing_roundtrip() {
        let (a, b) = tokio::io::duplex(4096);
        let msg = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"test"});

        let msg_str = serde_json::to_string(&msg).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", msg_str.len());
        let full = format!("{}{}", header, msg_str);

        let mut writer = a;
        let _ = writer.write_all(full.as_bytes()).await;
        drop(writer);

        let mut reader = BufReader::new(b);
        let mut content_length = 0;

        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(stripped) = line.strip_prefix("Content-Length:") {
                content_length = stripped.trim().parse().unwrap();
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await.unwrap();
        let got: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(got["method"], "test");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn silent_server_initialization_times_out_and_is_terminated() {
        let mut bridge = LspMcpBridge::new(LspServerConfig {
            language_id: "rust".into(),
            server_path: "/bin/sh".into(),
            server_args: vec!["-c".into(), "sleep 60".into()],
            workspace_root: "file:///tmp".into(),
        });

        let started = tokio::time::Instant::now();
        let result = bridge.initialize().await;

        assert!(
            matches!(result, Err(LspBridgeError::Communication(message)) if message.contains("timed out"))
        );
        assert!(started.elapsed() < tokio::time::Duration::from_secs(2));
        assert!(!bridge.is_initialized());
        assert!(bridge.child.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_initialization_is_terminated_without_waiting_for_timeout() {
        let mut bridge = LspMcpBridge::new(LspServerConfig {
            language_id: "rust".into(),
            server_path: "/bin/sh".into(),
            server_args: vec!["-c".into(), "exit 0".into()],
            workspace_root: "file:///tmp".into(),
        });

        let result = bridge.initialize().await;

        assert!(matches!(result, Err(LspBridgeError::Communication(_))));
        assert!(!bridge.is_initialized());
        assert!(bridge.child.is_none());
    }

    #[tokio::test]
    async fn outbound_messages_are_bounded_before_pipe_io() {
        let mut bridge = LspMcpBridge::new(LspServerConfig {
            language_id: "rust".into(),
            server_path: "unused".into(),
            server_args: Vec::new(),
            workspace_root: "file:///tmp".into(),
        });
        let oversized = serde_json::json!({
            "payload": "x".repeat(MAX_LSP_MESSAGE_BYTES + 1)
        });

        let result = bridge.write_message(&oversized).await;

        assert!(
            matches!(result, Err(LspBridgeError::Communication(message)) if message.contains("maximum"))
        );
    }

    #[tokio::test]
    async fn inbound_header_lines_are_bounded_before_allocation_growth() {
        let bytes = vec![b'x'; MAX_LSP_HEADER_LINE_BYTES + 1];
        let mut reader = tokio::io::BufReader::new(bytes.as_slice());

        let result = read_lsp_header_line_bounded(&mut reader).await;

        assert!(
            matches!(result, Err(LspBridgeError::Communication(message)) if message.contains("header line exceeds"))
        );
    }

    #[test]
    fn diagnostics_document_reads_are_bounded() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rightkit-agent-lsp-document-bound-{}-{unique}.rs",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((super::super::helpers::MAX_LSP_DOCUMENT_BYTES + 1) as u64)
            .unwrap();
        #[cfg(windows)]
        let uri = format!("file:///{}", path.to_string_lossy().replace('\\', "/"));
        #[cfg(not(windows))]
        let uri = format!("file://{}", path.display());

        let result = read_file_text_for_uri(&uri);

        assert!(
            matches!(result, Err(LspBridgeError::Communication(message)) if message.contains("diagnostics document") && message.contains("maximum"))
        );
        std::fs::remove_file(path).unwrap();
    }
}
