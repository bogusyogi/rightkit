//! Base vs add-on grouping is data on the spec: `catalog()` is the base set (as in 0.2.8),
//! add-ons come only from `add_ons()`, and rollups default to base.

use rightkit_models::suite::catalog::{add_ons, all_models, base_catalog, catalog, ModelTier};
use rightkit_models::suite::manifest::VerifyMode;
use rightkit_models::suite::store::{
    base_size_hint_bytes, base_status_at, set_status_at, size_hint_bytes, status_at, EntryState,
};
use rightkit_models::suite::{ensure_with_grant, ModelStore};

fn ids(specs: &[rightkit_models::suite::ModelSpec]) -> Vec<&'static str> {
    specs.iter().map(|s| s.id).collect()
}

#[test]
fn catalog_is_the_base_set_and_excludes_every_add_on() {
    assert_eq!(ids(&catalog()), ids(&base_catalog()));
    assert!(catalog()
        .iter()
        .all(|s| s.tier == ModelTier::Base && !s.is_add_on()));
    assert!(!catalog().iter().any(|s| s.id == "whisper-turbo"));
    // 0.2.8 returned Parakeet (plus OCR on Windows) and nothing else.
    let expected = if cfg!(windows) { 2 } else { 1 };
    assert_eq!(catalog().len(), expected);
    assert_eq!(
        catalog()[0].id,
        rightkit_models::suite::catalog::parakeet_spec().id
    );
}

#[test]
fn add_ons_are_marked_entitlement_gated_and_all_models_is_base_then_add_ons() {
    let add = add_ons();
    assert!(!add.is_empty());
    for spec in &add {
        assert_eq!(spec.tier, ModelTier::AddOn);
        assert!(spec.display_label().ends_with("(optional)"));
        assert_eq!(spec.missing_status_label(), "Optional, not installed");
        if spec.entitlement {
            assert!(spec.requires_entitlement(), "{}", spec.id);
            assert!(
                spec.requires_auth() && !spec.requires_grant(),
                "{}",
                spec.id
            );
        } else {
            // A public-read add-on (the macOS EmbeddingGemma 2 Core ML bundle): no bearer.
            assert!(
                !spec.requires_auth() && !spec.requires_entitlement(),
                "{}",
                spec.id
            );
        }
    }
    // Whisper stays first and entitlement-gated.
    assert_eq!(add[0].id, "whisper-turbo");
    assert!(add[0].requires_entitlement());
    let mut expected = ids(&base_catalog());
    expected.extend(ids(&add));
    assert_eq!(ids(&all_models()), expected);
    // Base models are grant-gated, never entitlement-gated.
    for spec in base_catalog() {
        assert!(!spec.requires_entitlement());
        assert_eq!(spec.missing_status_label(), "Not installed");
        assert_eq!(spec.display_label(), spec.label);
    }
}

#[test]
fn rollups_default_to_base_and_an_add_on_never_blocks_setup() {
    let root = tempfile::tempdir().unwrap();
    let base = base_status_at(root.path(), VerifyMode::Size);
    assert!(!base.ready, "nothing installed yet");
    assert_eq!(base.models.len(), base_catalog().len());
    assert!(base.models.iter().all(|m| m.tier == ModelTier::Base));
    assert_eq!(base.base_download_bytes, base_size_hint_bytes());
    assert_eq!(base.total_size_hint_bytes, base_size_hint_bytes());
    assert_eq!(base_size_hint_bytes(), size_hint_bytes(&base_catalog()));

    let everything = set_status_at(root.path(), &all_models(), VerifyMode::Size);
    // The add-on shows up in the listing and the total, not in what setup still needs.
    assert!(everything.total_size_hint_bytes > base_size_hint_bytes());
    assert_eq!(everything.base_download_bytes, base_size_hint_bytes());
    assert!(!everything.ready);
    let add_on = everything
        .models
        .iter()
        .find(|m| m.id == "whisper-turbo")
        .unwrap();
    assert_eq!(add_on.state, EntryState::Missing);
    assert_eq!(add_on.status_label(), "Optional, not installed");
    assert!(add_on.requires_entitlement);
    let parakeet = &everything.models[0];
    assert_eq!(parakeet.status_label(), "Not installed");

    // A set made only of add-ons is vacuously ready: a missing optional model is not incomplete setup.
    let only_add_ons = set_status_at(root.path(), &add_ons(), VerifyMode::Size);
    assert!(only_add_ons.ready);
    assert_eq!(only_add_ons.base_download_bytes, 0);
}

#[test]
fn grants_are_never_requested_for_an_entitlement_gated_model() {
    let root = tempfile::tempdir().unwrap();
    let spec = add_ons().remove(0);
    let err = ensure_with_grant(root.path(), &spec, "app", None, &|| false, |_| {}).unwrap_err();
    assert!(err.contains("entitlement"), "{err}");
    assert!(!err.contains("device id"), "no grant path was taken: {err}");
    assert_eq!(
        status_at(root.path(), &spec, VerifyMode::Size).state,
        EntryState::Missing
    );

    // ensure_entitled refuses a model that is not entitlement-gated.
    let store = ModelStore::at_models_root(root.path(), "app");
    let base = base_catalog().remove(0);
    let err = store
        .ensure_entitled(&base, "Bearer x", &|| false, |_| {})
        .unwrap_err();
    assert!(err.contains("does not use an entitlement bearer"), "{err}");
}
