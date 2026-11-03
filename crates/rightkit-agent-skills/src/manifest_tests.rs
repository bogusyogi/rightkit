mod tests {
    use super::super::*;

    const GOOD: &str = "+++\n\
name = \"git-guard\"\n\
description = \"Guards git commands\"\n\
trigger = \"event == \\\"PreToolUse\\\" and tool contains \\\"git\\\"\"\n\
enabled = true\n\
tags = [\"git\", \"safety\"]\n\
path_affinity = [\"*.rs\", \"src/**\"]\n\
+++\n\
\n\
# Body\n\
This part is ignored.\n";

    #[test]
    fn frontmatter_parses() {
        let m = SkillManifest::from_frontmatter(GOOD).unwrap();
        assert_eq!(m.name, "git-guard");
        assert_eq!(m.description, "Guards git commands");
        assert_eq!(
            m.trigger,
            "event == \"PreToolUse\" and tool contains \"git\""
        );
        assert!(m.enabled);
        assert_eq!(m.tags, vec!["git".to_string(), "safety".to_string()]);
        assert_eq!(
            m.path_affinity,
            vec!["*.rs".to_string(), "src/**".to_string()]
        );
    }

    #[test]
    fn frontmatter_missing_delimiters_errors() {
        let text = "name = \"x\"\ndescription = \"y\"\ntrigger = \"event\"\n";
        let err = SkillManifest::from_frontmatter(text).unwrap_err();
        assert!(matches!(err, SkillError::Frontmatter(_)));
    }

    #[test]
    fn manifest_validate_good_trigger() {
        let m = SkillManifest::from_frontmatter(GOOD).unwrap();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn manifest_validate_bad_trigger_errors() {
        let bad = "+++\n\
name = \"broken\"\n\
description = \"bad trigger\"\n\
trigger = \"event ==\"\n\
+++\n\
body\n";
        let m = SkillManifest::from_frontmatter(bad).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, SkillError::Trigger(_)));
    }

    #[test]
    fn manifest_serde_roundtrip() {
        let m = SkillManifest {
            name: "roundtrip".into(),
            description: "desc".into(),
            trigger: "event == \"Stop\"".into(),
            enabled: true,
            lifecycle: SkillLifecycle::Active,
            version: Some("1.0.0".into()),
            author: Some("Agent".into()),
            compatibility: vec!["rightkit-agent>=0.1".into()],
            platforms: vec!["windows".into(), "macos".into()],
            required_environment_variables: vec!["EXAMPLE_TOKEN".into()],
            metadata: SkillPackageMetadata {
                host: HostSkillMetadata {
                    category: Some(SkillCategory::Tool),
                    requires_toolsets: vec!["coding-local".into()],
                    fallback_for_toolsets: vec!["safe-research".into()],
                    config: serde_json::Value::Null,
                    gate_mode: Some(SkillGateMode::Hard),
                    benchmark_ids: vec!["skill-fixture".into()],
                },
            },
            category: SkillCategory::Tool,
            priority: default_skill_priority(),
            disable_model_invocation: false,
            tags: vec!["a".into(), "b".into()],
            path_affinity: vec!["*.md".into()],
            effects: SkillEffects::default(),
            body: String::new(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SkillManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    const ANTHROPIC_SKILL: &str = "---\n\
name: pdf-processing\n\
description: Extract text and tables from PDF files. Use when the user provides a PDF.\n\
---\n\
\n\
# PDF processing\n\
\n\
Use `pdftotext` to extract text, then parse tables.\n";

    #[test]
    fn parses_anthropic_skill_md_yaml() {
        let m = SkillManifest::from_markdown(ANTHROPIC_SKILL).unwrap();
        assert_eq!(m.name, "pdf-processing");
        assert!(m.description.starts_with("Extract text"));
        assert_eq!(m.trigger, "", "Anthropic skills have no trigger");
        assert!(m.enabled, "an authored SKILL.md is active by default");
        assert_eq!(m.lifecycle, SkillLifecycle::Active);
        assert!(m.path_affinity.is_empty());
        assert!(m.body.contains("pdftotext"), "body is the instructions");
        // No trigger → validate passes (model-invoked).
        assert!(m.validate().is_ok());
    }

    #[test]
    fn yaml_skill_md_accepts_open_metadata_without_env_context_leak() {
        let text = "---\n\
name: behavior-pack\n\
description: Improve implementation discipline.\n\
version: 2.1.0\n\
author: Example Team\n\
lifecycle: quarantined\n\
compatibility: [host>=0.1, anthropic-skill]\n\
platforms: [windows, macos, linux]\n\
required_environment_variables: [PRIVATE_API_KEY, CI_TOKEN]\n\
metadata:\n\
  host:\n\
    category: behavioral\n\
    requires_toolsets: [coding-local]\n\
    fallback_for_toolsets: [safe-research]\n\
    gate_mode: mixed\n\
    benchmark_ids: [behavior-diff-scope, rewrite-rate]\n\
---\n\
\n\
# Behavior Pack\n\
\n\
Keep changes surgical.\n";

        let m = SkillManifest::from_markdown(text).unwrap();

        assert_eq!(m.version.as_deref(), Some("2.1.0"));
        assert_eq!(m.author.as_deref(), Some("Example Team"));
        assert_eq!(m.lifecycle, SkillLifecycle::Quarantined);
        assert_eq!(
            m.compatibility,
            vec!["rightkit-agent>=0.1", "anthropic-skill"]
        );
        assert_eq!(m.platforms, vec!["windows", "macos", "linux"]);
        assert_eq!(
            m.required_environment_variables,
            vec!["PRIVATE_API_KEY", "CI_TOKEN"]
        );
        assert_eq!(m.metadata.host.category, Some(SkillCategory::Behavioral));
        assert_eq!(m.metadata.host.requires_toolsets, vec!["coding-local"]);
        assert_eq!(m.metadata.host.fallback_for_toolsets, vec!["safe-research"]);
        assert_eq!(m.metadata.host.gate_mode, Some(SkillGateMode::Mixed));
        assert_eq!(
            m.metadata.host.benchmark_ids,
            vec!["behavior-diff-scope", "rewrite-rate"]
        );
        assert!(!m.is_active());
        assert!(!m.body.contains("PRIVATE_API_KEY"));
    }

    #[test]
    fn toml_skill_md_accepts_host_metadata() {
        let text = "+++\n\
name = \"reviewer\"\n\
description = \"Review changes\"\n\
enabled = true\n\
lifecycle = \"active\"\n\
version = \"1.2.3\"\n\
author = \"Host\"\n\
compatibility = [\"host>=0.1\"]\n\
platforms = [\"windows\", \"macos\"]\n\
required_environment_variables = [\"REVIEW_TOKEN\"]\n\
[metadata.host]\n\
category = \"tool\"\n\
requires_toolsets = [\"coding-local\"]\n\
gate_mode = \"hard\"\n\
benchmark_ids = [\"review-fixture\"]\n\
+++\n\
\n\
# Reviewer\n";

        let m = SkillManifest::from_markdown(text).unwrap();

        assert!(m.is_active());
        assert_eq!(m.version.as_deref(), Some("1.2.3"));
        assert_eq!(m.metadata.host.gate_mode, Some(SkillGateMode::Hard));
        assert_eq!(m.metadata.host.benchmark_ids, vec!["review-fixture"]);
        assert_eq!(m.required_environment_variables, vec!["REVIEW_TOKEN"]);
    }

    #[test]
    fn from_markdown_dispatches_toml_too() {
        let m = SkillManifest::from_markdown(GOOD).unwrap();
        assert_eq!(m.name, "git-guard");
        assert!(!m.trigger.is_empty());
    }

    #[test]
    fn anthropic_skill_requires_name_and_description() {
        let missing_desc = "---\nname: x\n---\nbody\n";
        assert!(matches!(
            SkillManifest::from_markdown(missing_desc),
            Err(SkillError::Frontmatter(_))
        ));
    }

    #[test]
    fn parses_anthropic_path_affinity_alias() {
        let text = "---\n\
name: rust-review\n\
description: Review Rust files.\n\
pathAffinity: [\"*.rs\", \"crates/**\"]\n\
---\n\
body\n";
        let m = SkillManifest::from_markdown(text).unwrap();
        assert_eq!(
            m.path_affinity,
            vec!["*.rs".to_string(), "crates/**".to_string()]
        );
    }

    #[test]
    fn parses_native_skill_effects() {
        let text = "+++\n\
name = \"review-mode\"\n\
description = \"Use careful review mode.\"\n\
trigger = \"event == \\\"PreToolUse\\\"\"\n\
enabled = true\n\
[effects]\n\
executionMode = \"review\"\n\
allowedTools = [\"read_file\", \"git_diff\"]\n\
disallowedTools = [\"shell\"]\n\
prompt = \"Review before changing files.\"\n\
+++\n\
body\n";
        let m = SkillManifest::from_markdown(text).unwrap();
        assert_eq!(m.effects.execution_mode, "review");
        assert_eq!(
            m.effects.allowed_tools,
            vec!["read_file".to_string(), "git_diff".to_string()]
        );
        assert_eq!(m.effects.disallowed_tools, vec!["shell".to_string()]);
        assert_eq!(m.effects.prompt, "Review before changing files.");
    }

    #[test]
    fn parses_anthropic_skill_effect_aliases() {
        let text = "---\n\
name: rust-review\n\
description: Review Rust files.\n\
executionMode: careful\n\
allowedTools: [\"read_file\", \"git_diff\"]\n\
disallowedTools: [\"write_file\"]\n\
effectPrompt: Inspect before editing.\n\
---\n\
body\n";
        let m = SkillManifest::from_markdown(text).unwrap();
        assert_eq!(m.effects.execution_mode, "careful");
        assert_eq!(
            m.effects.allowed_tools,
            vec!["read_file".to_string(), "git_diff".to_string()]
        );
        assert_eq!(m.effects.disallowed_tools, vec!["write_file".to_string()]);
        assert_eq!(m.effects.prompt, "Inspect before editing.");
    }

    #[test]
    fn parses_behavioral_category_and_priority() {
        let native = "+++\n\
name = \"baseline-behavior\"\n\
description = \"Default coding behavior.\"\n\
category = \"behavioral\"\n\
priority = 25\n\
+++\n\
body\n";
        let m = SkillManifest::from_markdown(native).unwrap();
        assert_eq!(m.category, SkillCategory::Behavioral);
        assert_eq!(m.priority, 25);

        let yaml = "---\n\
name: baseline-behavior\n\
description: Default coding behavior.\n\
category: behavioral\n\
priority: 10\n\
---\n\
body\n";
        let m = SkillManifest::from_markdown(yaml).unwrap();
        assert_eq!(m.category, SkillCategory::Behavioral);
        assert_eq!(m.priority, 10);
    }

    #[test]
    fn parses_disable_model_invocation_aliases() {
        let native = "+++\n\
name = \"deploy\"\n\
description = \"Deploy workflow.\"\n\
disableModelInvocation = true\n\
+++\n\
body\n";
        let m = SkillManifest::from_markdown(native).unwrap();
        assert!(m.disable_model_invocation);

        let yaml = "---\n\
name: deploy\n\
description: Deploy workflow.\n\
disable-model-invocation: true\n\
---\n\
body\n";
        let m = SkillManifest::from_markdown(yaml).unwrap();
        assert!(m.disable_model_invocation);
    }

    #[test]
    fn normalize_skill_pack_github_url_converts_blob_to_raw() {
        let url = normalize_skill_pack_github_url(
            "https://github.com/owner/repo/blob/main/skills/my-skill.md",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://raw.githubusercontent.com/owner/repo/main/skills/my-skill.md"
        );
    }

    #[test]
    fn normalize_skill_pack_github_url_passes_through_raw() {
        let url = normalize_skill_pack_github_url(
            "https://raw.githubusercontent.com/owner/repo/v1.0/skills/my-skill.md",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://raw.githubusercontent.com/owner/repo/v1.0/skills/my-skill.md"
        );
    }

    #[test]
    fn normalize_skill_pack_github_url_rejects_empty() {
        let err = normalize_skill_pack_github_url("").unwrap_err();
        assert!(matches!(err, SkillError::Install(_)));
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn normalize_skill_pack_github_url_rejects_non_github_host() {
        let err = normalize_skill_pack_github_url(
            "https://gitlab.com/owner/repo/blob/main/skills/my-skill.md",
        )
        .unwrap_err();
        assert!(matches!(err, SkillError::Install(_)));
        assert!(err.to_string().contains("gitlab.com"));
    }

    #[test]
    fn normalize_skill_pack_github_url_rejects_malformed_blob_path() {
        let err = normalize_skill_pack_github_url(
            "https://github.com/owner/repo/tree/main/skills/my-skill.md",
        )
        .unwrap_err();
        assert!(matches!(err, SkillError::Install(_)));
        assert!(err.to_string().contains("blob"));
    }

    #[test]
    fn workspace_skill_discovery_dedupes_by_precedence() {
        let root = std::env::temp_dir().join(format!(
            "rightkit-agent-skill-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let user = root.join("user");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(user.join("review")).unwrap();
        std::fs::create_dir_all(workspace.join("review")).unwrap();
        let skill =
            |description: &str| format!("---\nname: review\ndescription: {description}\n---\nBody");
        std::fs::write(user.join("review/SKILL.md"), skill("user")).unwrap();
        std::fs::write(workspace.join("review/SKILL.md"), skill("workspace")).unwrap();

        let found = discover_skill_roots(&[
            (SkillSourceScope::User, user),
            (SkillSourceScope::Workspace, workspace.clone()),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.description, "workspace");
        assert_eq!(found[0].scope, SkillSourceScope::Workspace);
        assert!(found[0].path.starts_with(workspace.canonicalize().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
