use super::*;
use rightkit_transcript_library::history::HistoryRetention;

const TEST_LEGAL_CONTRACT: &str = r#"{"acceptanceVersion":"heardright-test-1","materialSha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;

fn configure_test_legal_contract() {
    configure_legal_contract(TEST_LEGAL_CONTRACT).unwrap();
}

#[test]
fn defaults_are_safe() {
    assert_eq!(DEFAULT_CLOSE_BEHAVIOR, "destroy");
    assert_eq!(DEFAULT_APP_THEME, "ember");
    assert_eq!(DEFAULT_APP_FONT, "hanken");
    assert_eq!(DEFAULT_ASR_BACKEND, "parakeet-tdt");
    assert_eq!(DEFAULT_PILL_VISIBILITY, "always");
    assert_eq!(DEFAULT_TRANSCRIPT_CLEANUP, "aggressive");
    const { assert!(!DEFAULT_AI_POLISH_ENABLED) };
    assert_eq!(DEFAULT_AI_POLISH_MODE, "app");
    const { assert!(DEFAULT_DIAGNOSTICS_ENABLED) };
    assert_eq!(DEFAULT_HISTORY_RETENTION, HistoryRetention::Forever);
    const { assert!(!DEFAULT_TELEMETRY_USAGE) };
    const { assert!(!DEFAULT_MEDIA_DUCKING_ON) };
}

#[test]
fn sync_provider_default_and_icloud_migration() {
    let blob = SettingsBlob::default();
    assert!(blob.sync_provider.is_none());
    assert_eq!(
        resolve_sync_provider(blob.sync_provider.as_deref(), false),
        "off"
    );
    // Existing iCloud-enabled user with no stored choice maps to icloud.
    assert_eq!(resolve_sync_provider(None, true), "icloud");
    // An explicit choice wins over the legacy iCloud consent.
    assert_eq!(resolve_sync_provider(Some("google"), true), "google");
    assert_eq!(resolve_sync_provider(Some("off"), true), "off");
    // Invalid stored value falls back to migration.
    assert_eq!(resolve_sync_provider(Some("bogus"), true), "icloud");
    let mut b = SettingsBlob::default();
    assert!(apply_patch_to(&mut b, &serde_json::json!({"sync_provider":"google"})).is_ok());
    assert!(apply_patch_to(&mut b, &serde_json::json!({"sync_provider":"dropbox"})).is_err());
    assert_eq!(b.sync_provider.as_deref(), Some("google"));
}

#[test]
fn patch_rejects_unknown_key() {
    let mut blob = SettingsBlob::default();
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "evil": "yes" })).is_err());
}

#[test]
fn patch_rejects_out_of_range_volume() {
    let mut blob = SettingsBlob::default();
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "sound_effects_volume": 9999 })
    )
    .is_err());
}

#[test]
fn patch_rejects_invalid_enum_values() {
    let mut blob = SettingsBlob::default();
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "close_behavior": "trash" })).is_err());
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "app_theme": "quantum" })).is_err());
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "asr_backend": "wav2vec" })).is_err());
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "asr_backend": "parakeet-unified" }),
    )
    .unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("parakeet-unified"));
    apply_patch_to(&mut blob, &serde_json::json!({ "asr_backend": "whisper" })).unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("whisper"));
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "asr_backend": "whisper-cli" }),
    )
    .unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("whisper-cli"));
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "asr_backend": "vulkan" })).is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "transcript_cleanup": "rewrite" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "transcript_cleanup": "minimal" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "transcript_cleanup": "maximal" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "ai_polish_mode": "summarize" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "ai_polish_mode": "simple" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "ai_polish_mode": "prompt" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "pill_visibility": "forever" })
    )
    .is_err());
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "history_retention": "two_weeks" })
    )
    .is_err());
}

#[test]
fn patch_rejects_wrong_typed_booleans() {
    let mut blob = SettingsBlob::default();
    let r = apply_patch_to(
        &mut blob,
        &serde_json::json!({ "diagnostics_enabled": "true" }),
    );
    assert!(r.is_err());
    assert_eq!(blob.diagnostics_enabled, None);
}

#[test]
fn patch_applies_valid_values() {
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({
            "diagnostics_enabled": false,
            "history_retention": "30_days",
            "sound_effects_volume": 80,
            "media_ducking_on": true,
            "asr_backend": "parakeet-tdt",
            "transcript_cleanup": "aggressive",
            "ai_polish_enabled": true,
            "ai_polish_mode": "app",
            "app_theme": "ember-light",
            "app_font": "hanken",
            "pill_visibility": "during_dictation",
            "snippets": { "/email": "adrian@example.com" }
        }),
    )
    .unwrap();
    assert_eq!(blob.diagnostics_enabled, Some(false));
    assert_eq!(blob.history_retention, Some(HistoryRetention::ThirtyDays));
    assert_eq!(blob.sound_effects_volume, Some(80));
    assert_eq!(blob.media_ducking_on, Some(true));
    assert_eq!(blob.asr_backend.as_deref(), Some("parakeet-tdt"));
    assert_eq!(blob.transcript_cleanup.as_deref(), Some("aggressive"));
    assert_eq!(blob.ai_polish_enabled, Some(true));
    assert_eq!(blob.ai_polish_mode.as_deref(), Some("app"));
    assert_eq!(blob.app_theme.as_deref(), Some("ember-light"));
    assert_eq!(blob.app_font.as_deref(), Some("hanken"));
    assert_eq!(blob.pill_visibility.as_deref(), Some("during_dictation"));
    assert_eq!(
        blob.snippets
            .as_ref()
            .and_then(|snippets| snippets.get("email"))
            .map(String::as_str),
        Some("adrian@example.com")
    );
}

#[test]
fn patch_rejects_when_a_later_key_is_invalid_after_earlier_applied() {
    // All-or-nothing: the valid key must NOT be committed when a later
    // key fails validation.
    let mut blob = SettingsBlob::default();
    let r = apply_patch_to(
        &mut blob,
        &serde_json::json!({ "diagnostics_enabled": false, "recording_style": "mini" }),
    );
    assert!(r.is_err());
    assert_eq!(blob.diagnostics_enabled, None);
}

#[test]
fn migrate_stamps_unversioned_blob_and_preserves_fields() {
    // A v0 (pre-versioning) blob with real user data must keep that data and
    // gain the current version — NOT get reset.
    let mut blob = SettingsBlob {
        diagnostics_enabled: Some(false),
        app_theme: Some("ink-copper".to_string()),
        schema_version: None,
        ..Default::default()
    };
    let changed = migrate(&mut blob);
    assert!(changed);
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.diagnostics_enabled, Some(false));
    assert_eq!(blob.app_theme.as_deref(), Some("ink-copper"));
}

#[test]
fn migrate_drops_retired_noop_settings_but_keeps_future_extras() {
    let json = serde_json::json!({
        "schema_version": 1,
        "privacy_mode": true,
        "recording_style": "classic",
        "launch_on_login": true,
        "telemetry_wake_samples": true,
        "future_key_xyz": 42
    });
    let mut blob: SettingsBlob = serde_json::from_value(json).unwrap();

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.diagnostics_enabled, Some(false));
    assert!(!blob.extras.contains_key("privacy_mode"));
    assert_eq!(
        blob.extras.get("future_key_xyz"),
        Some(&serde_json::json!(42))
    );
    assert!(!blob.extras.contains_key("recording_style"));
    assert!(!blob.extras.contains_key("launch_on_login"));
    assert!(!blob.extras.contains_key("telemetry_wake_samples"));
}

#[test]
fn migrate_canonicalizes_legacy_parakeet_alias_to_current_default() {
    let mut blob = SettingsBlob {
        schema_version: Some(2),
        asr_backend: Some("parakeet".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.asr_backend.as_deref(), Some(DEFAULT_ASR_BACKEND));
}

#[test]
fn migrate_retires_explicit_rnnt_choice() {
    let mut blob = SettingsBlob {
        schema_version: Some(4),
        asr_backend: Some("parakeet-rnnt".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.asr_backend.as_deref(), Some(DEFAULT_ASR_BACKEND));
}

#[test]
fn migrate_v6_to_v7_moves_tdt_default_to_unified() {
    let mut blob = SettingsBlob {
        schema_version: Some(6),
        asr_backend: Some("parakeet-tdt".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.asr_backend.as_deref(), Some(DEFAULT_ASR_BACKEND));
}

#[test]
fn migrate_v6_to_v7_preserves_whisper_choice() {
    let mut blob = SettingsBlob {
        schema_version: Some(6),
        asr_backend: Some("whisper".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.asr_backend.as_deref(), Some("whisper"));
}

#[test]
fn migrate_v7_to_v8_retires_prompt_as_persistent_ai_polish_mode() {
    let mut blob = SettingsBlob {
        schema_version: Some(7),
        ai_polish_mode: Some("prompt".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.ai_polish_mode.as_deref(), Some(DEFAULT_AI_POLISH_MODE));
}

#[test]
fn migrate_is_idempotent_noop_on_current_version() {
    let mut blob = SettingsBlob {
        schema_version: Some(CURRENT_SCHEMA_VERSION),
        ..Default::default()
    };
    assert!(!migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
}

#[test]
fn migrate_v5_to_v6_folds_old_simple_ai_polish_into_app_l1() {
    let mut blob = SettingsBlob {
        schema_version: Some(5),
        ai_polish_mode: Some("simple".to_string()),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
    assert_eq!(blob.ai_polish_mode.as_deref(), Some("app"));
}

#[test]
fn migrate_v3_to_v4_sets_ephemeral_hub_default_for_fresh_installs() {
    // A v3 blob with NO close_behavior set is a fresh install. After
    // migration the new world-class default is `destroy` (the hub is
    // ephemeral; the engine is the only long-lived resident).
    let mut blob = SettingsBlob {
        schema_version: Some(3),
        ..Default::default()
    };
    assert!(migrate(&mut blob));
    assert_eq!(blob.close_behavior.as_deref(), Some("destroy"));
    assert_eq!(
        blob.pill_visibility.as_deref(),
        Some("always"),
        "fresh installs default to always-visible pill"
    );
    assert_eq!(blob.schema_version, Some(CURRENT_SCHEMA_VERSION));
}

#[test]
fn migrate_v3_to_v4_preserves_existing_user_close_behavior() {
    // A v3 blob that ALREADY has close_behavior set keeps the stored
    // value. Existing users don't get their hub-lifecycle silently
    // changed.
    let mut blob = SettingsBlob {
        schema_version: Some(3),
        close_behavior: Some("minimize".to_string()),
        ..Default::default()
    };
    assert!(migrate(&mut blob));
    assert_eq!(blob.close_behavior.as_deref(), Some("minimize"));
}

#[test]
fn migrate_v3_to_v4_preserves_existing_pill_visibility() {
    let mut blob = SettingsBlob {
        schema_version: Some(3),
        pill_visibility: Some("always".to_string()),
        ..Default::default()
    };
    assert!(migrate(&mut blob));
    assert_eq!(blob.pill_visibility.as_deref(), Some("always"));
}

#[test]
fn patch_accepts_recording_only_pill_visibility() {
    // The new spec-locked value `recording_only` must be accepted by
    // the allowlist. The legacy `during_dictation` alias remains
    // accepted for backward-compat round-trips.
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "pill_visibility": "recording_only" }),
    )
    .unwrap();
    assert_eq!(blob.pill_visibility.as_deref(), Some("recording_only"));
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "pill_visibility": "during_dictation" }),
    )
    .unwrap();
    assert_eq!(blob.pill_visibility.as_deref(), Some("during_dictation"));
}

#[test]
fn patch_accepts_only_supported_macos_ptt_keys() {
    for key in ["ctrl", "lalt", "ralt"] {
        let mut blob = SettingsBlob::default();
        apply_patch_to(
            &mut blob,
            &serde_json::json!({ "mac_push_to_talk_key": key }),
        )
        .unwrap();
        assert_eq!(blob.mac_push_to_talk_key.as_deref(), Some(key));
    }

    let mut blob = SettingsBlob::default();
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "mac_push_to_talk_key": "command" }),
    )
    .is_err());
}

#[test]
fn patch_accepts_supported_windows_ptt_keys() {
    for key in ["lalt", "ralt", "rctrl"] {
        let mut blob = SettingsBlob::default();
        apply_patch_to(&mut blob, &serde_json::json!({ "push_to_talk_key": key })).unwrap();
        assert_eq!(blob.push_to_talk_key.as_deref(), Some(key));
    }
}

#[test]
fn patch_accepts_destroy_close_behavior() {
    // The new spec-locked value `destroy` must be accepted by the
    // allowlist.
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "close_behavior": "destroy" }),
    )
    .unwrap();
    assert_eq!(blob.close_behavior.as_deref(), Some("destroy"));
}

#[test]
fn patch_accepts_asr_aliases_for_runtime_routing() {
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "asr_backend": "parakeet-unified" }),
    )
    .unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("parakeet-unified"));

    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "asr_backend": "parakeet-rnnt" }),
    )
    .unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("parakeet-rnnt"));

    apply_patch_to(&mut blob, &serde_json::json!({ "asr_backend": "rnnt" })).unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("rnnt"));

    apply_patch_to(&mut blob, &serde_json::json!({ "asr_backend": "whisper" })).unwrap();
    assert_eq!(blob.asr_backend.as_deref(), Some("whisper"));
}

#[test]
fn unknown_keys_survive_round_trip_in_extras() {
    // Forward-compat: a future key written by a newer build must not be lost
    // when an older build loads + re-saves the blob.
    let json = serde_json::json!({ "diagnostics_enabled": false, "future_key_xyz": 42 });
    let blob: SettingsBlob = serde_json::from_value(json).unwrap();
    assert_eq!(blob.diagnostics_enabled, Some(false));
    let back = serde_json::to_value(&blob).unwrap();
    assert_eq!(back.get("future_key_xyz"), Some(&serde_json::json!(42)));
}

#[test]
fn schema_version_is_not_user_patchable() {
    // schema_version is internal — a patch trying to set it is rejected by
    // the allowlist, so a client can't forge the version.
    let mut blob = SettingsBlob::default();
    let r = apply_patch_to(&mut blob, &serde_json::json!({ "schema_version": 99 }));
    assert!(r.is_err());
    assert_eq!(blob.schema_version, None);
}

#[test]
fn delivery_method_default_is_paste() {
    assert_eq!(DEFAULT_DELIVERY_METHOD, "paste");
}

#[test]
fn apply_patch_accepts_valid_delivery_method_and_rejects_others() {
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "delivery_method": "keystroke" }),
    )
    .unwrap();
    assert_eq!(blob.delivery_method.as_deref(), Some("keystroke"));
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "delivery_method": "paste" }),
    )
    .unwrap();
    assert_eq!(blob.delivery_method.as_deref(), Some("paste"));
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "delivery_method": "bogus" })
    )
    .is_err());
    // Rejected value left the prior value intact (all-or-nothing patch).
    assert_eq!(blob.delivery_method.as_deref(), Some("paste"));
}

#[test]
fn screenshot_prompt_selection_is_native_settings_state() {
    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({
            "screenshot_destination": "disk",
            "screenshot_prompt_selected": true
        }),
    )
    .unwrap();
    assert_eq!(blob.screenshot_destination.as_deref(), Some("disk"));
    assert_eq!(blob.screenshot_prompt_selected, Some(true));
    assert!(apply_patch_to(
        &mut blob,
        &serde_json::json!({ "screenshot_prompt_selected": "yes" })
    )
    .is_err());
    assert_eq!(blob.screenshot_prompt_selected, Some(true));
}

#[test]
fn screenshot_prompt_can_only_be_claimed_once() {
    let mut blob = SettingsBlob::default();
    assert!(blob.claim_screenshot_prompt());
    assert_eq!(blob.screenshot_prompt_selected, Some(true));
    assert!(!blob.claim_screenshot_prompt());
}

#[test]
fn cancel_hint_seen_is_native_settings_state_and_claimed_once() {
    let mut blob = SettingsBlob::default();
    assert_eq!(blob.cancel_hint_seen, None);
    assert!(blob.claim_cancel_hint());
    assert_eq!(blob.cancel_hint_seen, Some(true));
    assert!(
        !blob.claim_cancel_hint(),
        "a second claim must not re-trigger the hint"
    );
}

#[test]
fn cancel_hint_seen_is_patchable_through_apply_patch() {
    let mut blob = SettingsBlob::default();
    apply_patch_to(&mut blob, &serde_json::json!({ "cancel_hint_seen": true })).unwrap();
    assert_eq!(blob.cancel_hint_seen, Some(true));
    assert!(apply_patch_to(&mut blob, &serde_json::json!({ "cancel_hint_seen": "yes" })).is_err());
    assert_eq!(blob.cancel_hint_seen, Some(true));
}

#[test]
fn migration_never_forces_completed_users_back_into_onboarding() {
    let mut blob = SettingsBlob {
        schema_version: Some(8),
        onboarding_complete: Some(true),
        onboarding_version: Some(2),
        ..Default::default()
    };

    assert!(migrate(&mut blob));
    assert_eq!(blob.onboarding_complete, Some(true));
    assert_eq!(blob.onboarding_version, Some(REQUIRED_ONBOARDING_VERSION));
}

#[test]
fn legal_acceptance_is_current_only_for_the_exact_required_version_and_fields() {
    configure_test_legal_contract();
    let mut blob = SettingsBlob {
        onboarding_complete: Some(true),
        ..Default::default()
    };
    assert!(!blob.legal_acceptance_current());

    apply_patch_to(
        &mut blob,
        &serde_json::json!({
            "legal_acceptance_version": required_legal_acceptance_version(),
            "legal_accepted_at": "2026-07-16T12:34:56.000Z",
            "legal_eligibility_basis": "enterprise",
            "legal_material_sha256": required_legal_material_sha256()
        }),
    )
    .unwrap();
    assert!(blob.legal_acceptance_current());

    blob.legal_acceptance_version = Some("heardright-old".to_string());
    assert!(!blob.legal_acceptance_current());
}

#[test]
fn legal_acceptance_patch_rejects_invalid_eligibility_or_hash_atomically() {
    configure_test_legal_contract();
    let mut blob = SettingsBlob::default();
    let bad_basis = apply_patch_to(
        &mut blob,
        &serde_json::json!({
            "legal_acceptance_version": required_legal_acceptance_version(),
            "legal_eligibility_basis": "company_10_plus_without_order"
        }),
    );
    assert!(bad_basis.is_err());
    assert_eq!(blob.legal_acceptance_version, None);

    let bad_hash = apply_patch_to(
        &mut blob,
        &serde_json::json!({ "legal_material_sha256": "not-a-sha256" }),
    );
    assert!(bad_hash.is_err());
    assert_eq!(blob.legal_material_sha256, None);

    let arbitrary_hash = apply_patch_to(
        &mut blob,
        &serde_json::json!({
            "legal_material_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }),
    );
    assert!(arbitrary_hash.is_err());
    assert_eq!(blob.legal_material_sha256, None);
}

#[test]
fn legal_acceptance_patch_requires_a_real_canonical_utc_timestamp() {
    for invalid in [
        "not-a-date-but-ends-in-Z",
        "2026-02-30T12:34:56.000Z",
        "2026-07-16T24:00:00.000Z",
        "2026-07-16T12:34:56Z",
        "2026-07-16T12:34:56.000+00:00",
    ] {
        let mut blob = SettingsBlob::default();
        let result = apply_patch_to(
            &mut blob,
            &serde_json::json!({ "legal_accepted_at": invalid }),
        );
        assert!(result.is_err(), "accepted invalid timestamp: {invalid}");
        assert_eq!(blob.legal_accepted_at, None);
    }

    let mut blob = SettingsBlob::default();
    apply_patch_to(
        &mut blob,
        &serde_json::json!({ "legal_accepted_at": "2024-02-29T23:59:59.999Z" }),
    )
    .unwrap();
    assert_eq!(
        blob.legal_accepted_at.as_deref(),
        Some("2024-02-29T23:59:59.999Z")
    );
}

#[test]
fn launch_time_keys_need_an_engine_restart_and_others_do_not() {
    use serde_json::json;
    assert!(patch_requires_engine_restart(
        &json!({"asr_backend": "parakeet-tdt"})
    ));
    assert!(patch_requires_engine_restart(
        &json!({"asr_compute_profile": "gpu"})
    ));
    assert!(!patch_requires_engine_restart(
        &json!({"pill_visibility": "always"})
    ));
    assert!(!patch_requires_engine_restart(&json!("asr_backend")));
}
