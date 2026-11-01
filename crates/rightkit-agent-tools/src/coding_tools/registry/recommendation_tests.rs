use super::recommendation::{toolset_size_warning, TOOLSET_SIZE_THRESHOLD};

#[test]
fn under_threshold_returns_none() {
    // 20 tools is below the 25 threshold
    assert!(toolset_size_warning(20).is_none());
}

#[test]
fn exactly_at_threshold_returns_none() {
    // Exactly at threshold is not over
    assert!(toolset_size_warning(TOOLSET_SIZE_THRESHOLD).is_none());
}

#[test]
fn one_over_threshold_returns_warning() {
    let warning =
        toolset_size_warning(TOOLSET_SIZE_THRESHOLD + 1).expect("should warn when over threshold");
    assert_eq!(warning.active_count, TOOLSET_SIZE_THRESHOLD + 1);
    assert_eq!(warning.threshold, TOOLSET_SIZE_THRESHOLD);
    assert!(warning.message.contains(&warning.active_count.to_string()));
    assert!(warning.message.contains(&warning.threshold.to_string()));
    assert!(warning.message.contains("--toolset") || warning.message.contains("--allowed-tools"));
}

#[test]
fn full_default_registry_triggers_warning() {
    // The full default registry has ~45+ tools, well over 25
    let reg = crate::coding_tools::ToolRegistry::build_default();
    let count = reg.all().len();
    let warning = toolset_size_warning(count);
    assert!(
        warning.is_some(),
        "default registry with {count} tools should exceed threshold {TOOLSET_SIZE_THRESHOLD}"
    );
}

#[test]
fn read_only_toolset_suppresses_warning() {
    // The read-only profile narrows tools below the threshold
    let reg = crate::coding_tools::ToolRegistry::build_for_allowed(&[
        "read_file".into(),
        "search".into(),
        "web".into(),
        "git".into(),
    ]);
    let count = reg.all().len();
    let warning = toolset_size_warning(count);
    assert!(
        warning.is_none(),
        "read-only toolset with {count} tools should be under threshold {TOOLSET_SIZE_THRESHOLD}"
    );
}
