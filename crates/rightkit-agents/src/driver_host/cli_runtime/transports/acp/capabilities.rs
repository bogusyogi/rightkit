//! ACP initialize capability advertisement. PR 7 fills in the
//! concrete client; this stub exists so adapters can compile
//! against the module path.

/// The the host app client identity advertised to the provider during
/// `initialize`. Kept constant so log forensics always see the
/// same value.
pub const HOST_CLIENT_NAME: &str = "rightkit-agent";

/// ACP client capabilities the host app advertises. The two important
/// fields are `fs.readTextFile` and `fs.writeTextFile` set to
/// `false`, plus `terminal` set to `false`: these advertise that
/// the host app will not perform any direct filesystem or terminal
/// operation through ACP, forcing the provider to use the host app MCP
/// for every effect.
pub const HOST_CLIENT_CAPABILITIES: &str = r#"{
  "fs": { "readTextFile": false, "writeTextFile": false },
  "terminal": false
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_capabilities_mark_filesystem_and_terminal_unavailable() {
        assert!(HOST_CLIENT_CAPABILITIES.contains("\"terminal\": false"));
        assert!(HOST_CLIENT_CAPABILITIES.contains("\"readTextFile\": false"));
        assert!(HOST_CLIENT_CAPABILITIES.contains("\"writeTextFile\": false"));
    }
}
