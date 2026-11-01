use super::*;

#[test]
fn matches_common_app_names() {
    let apps: Vec<String> = [
        "Google Chrome",
        "Figma",
        "Visual Studio Code",
        "Spotify",
        "Mail",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        best_match("chrome", &apps).as_deref(),
        Some("Google Chrome")
    );
    assert_eq!(best_match("figma", &apps).as_deref(), Some("Figma"));
    assert_eq!(
        best_match("code", &apps).as_deref(),
        Some("Visual Studio Code")
    );
    assert_eq!(best_match("spotify", &apps).as_deref(), Some("Spotify"));
    // Mishear still resolves.
    assert_eq!(best_match("spotofy", &apps).as_deref(), Some("Spotify"));
}

#[test]
fn stray_dictation_finds_nothing() {
    let apps: Vec<String> = ["Google Chrome", "Figma"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(best_match("the door", &apps), None);
    assert_eq!(best_match("my report", &apps), None);
}

#[test]
fn short_common_words_do_not_collide() {
    // Real installed-app names that previously mis-fired on short words.
    let apps: Vec<String> = [
        "Microsoft Teams",
        "Microsoft Edge",
        "iTerm2",
        "Logic Pro",
        "Script Editor",
        "Zoom",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for word in ["mic", "it", "pro", "edit", "loom"] {
        assert_eq!(best_match(word, &apps), None, "{word:?} should not launch");
    }
    // But real app words still resolve.
    assert_eq!(best_match("zoom", &apps).as_deref(), Some("Zoom"));
    assert_eq!(
        best_match("teams", &apps).as_deref(),
        Some("Microsoft Teams")
    );
}

#[test]
fn streaming_shortcut_lookup_requires_ready_unique_exact_catalog_match() {
    let _guard = crate::test_support::test_env_guard();

    set_shortcut_cache_for_tests(None);
    assert_eq!(resolve_shortcut_streaming("good night"), None);

    set_shortcut_cache_for_tests(Some(vec!["Good Night".to_string()]));
    assert_eq!(
        resolve_shortcut_streaming("good night").as_deref(),
        Some("Good Night")
    );
    assert_eq!(resolve_shortcut_streaming("good"), None);
    assert_eq!(resolve_shortcut_streaming("unknown"), None);

    // Spacing/punctuation equivalence is safe only while it identifies one
    // installed shortcut; collisions stay pending for final resolution.
    set_shortcut_cache_for_tests(Some(vec![
        "Good-Night".to_string(),
        "Good Night".to_string(),
    ]));
    assert_eq!(resolve_shortcut_streaming("good night"), None);

    // A refreshed snapshot exposes newly-created shortcuts to next probe.
    set_shortcut_cache_for_tests(Some(vec!["Water Eject".to_string()]));
    assert_eq!(
        resolve_shortcut_streaming("water eject").as_deref(),
        Some("Water Eject")
    );

    set_shortcut_cache_for_tests(Some(vec!["Good".to_string(), "Good Night".to_string()]));
    assert_eq!(resolve_shortcut_streaming("good").as_deref(), Some("Good"));
    assert!(shortcut_streaming_is_ambiguous("good"));
}

#[test]
fn failed_refresh_does_not_make_old_snapshot_fire_again() {
    let _guard = crate::test_support::test_env_guard();
    set_shortcut_cache_for_tests(Some(vec!["Good Night".to_string()]));
    let old_success = std::time::Instant::now()
        .checked_sub(SHORTCUT_REFRESH_INTERVAL + std::time::Duration::from_secs(1));
    set_shortcut_refresh_times_for_tests(old_success, Some(std::time::Instant::now()));

    assert!(shortcut_cache_is_stale());
    // A failed attempt advances throttle time only; successful snapshot age
    // remains old, so streaming keeps waiting for a retry.
    set_shortcut_refresh_times_for_tests(old_success, Some(std::time::Instant::now()));
    assert!(shortcut_cache_is_stale());
}

#[test]
fn stale_background_generation_cannot_replace_authoritative_snapshot() {
    let _guard = crate::test_support::test_env_guard();
    set_shortcut_cache_for_tests(Some(vec!["Current".to_string()]));
    let generation = MAC_SHORTCUT_CACHE_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    MAC_SHORTCUT_CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    store_shortcut_cache(generation, vec!["Stale".to_string()]);

    assert_eq!(
        resolve_shortcut_streaming("current").as_deref(),
        Some("Current")
    );
    assert_eq!(resolve_shortcut_streaming("stale"), None);
}
