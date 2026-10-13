//! Feature-split guard: the database layer lives only behind `store`, and a build without it
//! still exposes the supervision and wire layers.

#[test]
fn only_the_store_feature_pulls_the_database() {
    let manifest = include_str!("../Cargo.toml");
    let dep = manifest
        .lines()
        .find(|l| l.starts_with("rightkit-db "))
        .expect("rightkit-db dependency line");
    assert!(
        dep.contains("optional = true"),
        "rightkit-db must stay optional: {dep}"
    );
    let enabling: Vec<&str> = manifest
        .lines()
        .filter(|l| l.contains("dep:rightkit-db"))
        .collect();
    assert_eq!(
        enabling.len(),
        1,
        "one feature may enable rightkit-db: {enabling:?}"
    );
    assert!(enabling[0].starts_with("store ="), "{}", enabling[0]);
    // The supervisor must not imply `store` or `contract` (which implies it).
    let sup = manifest
        .lines()
        .find(|l| l.starts_with("supervisor ="))
        .expect("supervisor feature");
    assert!(!sup.contains("store") && !sup.contains("contract"), "{sup}");
}

#[cfg(not(feature = "store"))]
#[test]
fn the_store_free_surface_has_no_database_types() {
    let e = rightkit_jobs::JobError::new("x", "y");
    assert_eq!(e.code, "x");
}

#[cfg(feature = "store")]
#[test]
fn the_store_surface_is_unchanged() {
    let _: Option<rightkit_jobs::OpenFailure> =
        rightkit_jobs::JobError::new("x", "y").open_failure();
    let _ = std::any::type_name::<rightkit_jobs::JobStore>();
    let _ = std::any::type_name::<rightkit_jobs::db::DbOptions>();
}

#[cfg(feature = "protocol")]
#[test]
fn wire_constants_are_available_with_protocol() {
    assert_eq!(rightkit_jobs::CONTRACT_VERSION, 1);
    assert_eq!(rightkit_jobs::DEFAULT_FRAME_LIMIT, 16 * 1024 * 1024);
}
