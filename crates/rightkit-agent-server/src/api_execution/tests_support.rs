#[cfg(test)]
mod tests_support {
    use super::*;
    pub(super) fn test_workspace(label: &str) -> PathBuf {
        let _ = rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join("coderight-test-tmp")
            .join(format!("execution-{label}-{nanos}"));
        if let Err(e) = std::fs::create_dir_all(&root) {
            panic!(
                "{}",
                crate::test_support::env_denied_message("create the test workspace", &root, &e)
            );
        }
        root
    }

    pub(super) fn encode_query_value(value: &str) -> String {
        value
            .bytes()
            .flat_map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    vec![byte as char]
                }
                other => format!("%{other:02X}").chars().collect(),
            })
            .collect()
    }
}
