use super::*;
use std::time::Duration;

#[test]
fn bounded_output_retains_exact_head_and_tail_budget() {
    let mut output = BoundedOutput::default();
    output.push(&vec![b'a'; HEAD_BYTES]);
    output.push(&vec![b'b'; TAIL_BYTES + 1]);
    assert!(output.truncated());
    let preview = output.preview_bytes();
    assert!(preview.starts_with(&vec![b'a'; HEAD_BYTES]));
    assert!(preview.ends_with(&vec![b'b'; TAIL_BYTES]));
    assert_eq!(output.total_bytes(), (RETAINED_BYTES + 1) as u64);
}

#[test]
fn registry_single_flights_matching_key_and_evicts_idle_entry() {
    let registry = SharedServiceRegistry::new(1);
    let key = ServiceKey::new("mcp", ServiceScope::Workspace("w".into()), "same");
    let first = registry.acquire(key.clone(), || 7usize).unwrap();
    let second = registry.acquire(key.clone(), || 8usize).unwrap();
    assert_eq!(*first.value(), 7);
    assert_eq!(*second.value(), 7);
    assert_eq!(registry.current_epoch(&key), Some(first.generation()));
    assert_eq!(registry.snapshot()[0].leases, 2);
    drop(first);
    drop(second);
    assert_eq!(registry.evict_idle(Duration::ZERO), vec![key.clone()]);
    assert_eq!(registry.current_epoch(&key), None);
}

#[test]
fn governor_releases_lease_capacity_and_rejects_session_overage() {
    let governor = ResourceGovernor::new(ResourceLimits::default());
    let scope = ServiceScope::Session("s".into());
    let lease = governor
        .acquire(ResourceKind::ChildProcess, scope.clone(), 16, true)
        .unwrap();
    assert!(governor
        .acquire(ResourceKind::ChildProcess, scope.clone(), 1, true)
        .is_err());
    drop(lease);
    assert!(governor
        .acquire(ResourceKind::ChildProcess, scope, 1, true)
        .is_ok());
}

#[test]
fn resource_profiles_are_explicit_and_practical_is_the_product_default() {
    assert_eq!(ResourceProfile::default(), ResourceProfile::Practical);
    assert_eq!(
        ResourceProfile::parse("strict").unwrap(),
        ResourceProfile::Strict
    );
    assert_eq!(
        ResourceProfile::parse("local").unwrap(),
        ResourceProfile::Practical
    );
    assert_eq!(
        ResourceProfile::parse("service").unwrap(),
        ResourceProfile::Expanded
    );
    assert!(ResourceProfile::parse("unbounded").is_err());
}

#[test]
fn strict_profile_types_every_process_backed_capability_unavailable_before_spawn() {
    let governor = ResourceGovernor::for_profile(ResourceProfile::Strict);
    let scope = ServiceScope::Session("strict".into());
    for kind in [
        ResourceKind::ExternalExecution,
        ResourceKind::ChildProcess,
        ResourceKind::BackgroundProcess,
        ResourceKind::Mcp,
        ResourceKind::Lsp,
        ResourceKind::BrowserProcess,
        ResourceKind::BrowserContext,
    ] {
        assert!(kind.is_process_backed());
        assert!(
            !governor.capability_available(kind),
            "{kind:?} must be unavailable"
        );
        let error = match governor.acquire(kind, scope.clone(), 1, true) {
            Ok(_) => panic!("strict profile admitted process-backed kind {kind:?}"),
            Err(error) => error,
        };
        assert_eq!(error.limit, 0);
        assert_eq!(error.kind, kind);
    }
    assert!(governor.capability_available(ResourceKind::Inference));
    assert!(governor
        .acquire(ResourceKind::Inference, scope, 1, true)
        .is_ok());
}

#[test]
fn practical_profile_caps_the_whole_external_execution_group_to_one() {
    let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
    let first_scope = ServiceScope::Session("a".into());
    let second_scope = ServiceScope::Session("b".into());
    let permit = governor
        .acquire(ResourceKind::ExternalExecution, first_scope, 1, true)
        .expect("first practical external execution group is admitted");
    let denied = match governor.acquire(
        ResourceKind::ExternalExecution,
        second_scope.clone(),
        1,
        true,
    ) {
        Ok(_) => panic!("practical profile admitted a second external execution group"),
        Err(error) => error,
    };
    assert_eq!(denied.limit, 1);
    drop(permit);
    assert!(governor
        .acquire(ResourceKind::ExternalExecution, second_scope, 1, true)
        .is_ok());
}

#[test]
fn expanded_profile_retains_finite_bounded_helper_pools() {
    let governor = ResourceGovernor::for_profile(ResourceProfile::Expanded);
    assert_eq!(governor.profile(), ResourceProfile::Expanded);
    assert_eq!(governor.limits(), ResourceLimits::default());
    let scope = ServiceScope::Session("expanded".into());
    assert!(governor
        .acquire(ResourceKind::ExternalExecution, scope.clone(), 16, true)
        .is_ok());
    assert!(governor
        .acquire(ResourceKind::ExternalExecution, scope, 17, true)
        .is_err());
}

#[test]
fn spool_publishes_content_addressed_synced_segment() {
    let directory =
        std::env::temp_dir().join(format!("rightkit-runtime-resources-{}", std::process::id()));
    let mut spool = OutputSpool::create(&directory).unwrap();
    spool.push(b"bounded raw bytes").unwrap();
    let reference = spool.finish(false).unwrap();
    assert_eq!(reference.size_bytes, 17);
    assert_eq!(reference.segments.len(), 1);
    assert_eq!(
        std::fs::read(&reference.segments[0]).unwrap(),
        b"bounded raw bytes"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn spool_temp_creation_is_collision_safe_under_parallel_writers() {
    let directory = std::env::temp_dir().join(format!(
        "rightkit-runtime-resources-parallel-{}",
        std::process::id()
    ));
    let handles = (0..16)
        .map(|_| {
            let directory = directory.clone();
            std::thread::spawn(move || {
                let mut spool = OutputSpool::create(directory).unwrap();
                spool.push(b"same content").unwrap();
                spool.finish(false).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let references = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert!(references
        .iter()
        .all(|reference| reference.id == references[0].id));
    assert_eq!(
        std::fs::read(&references[0].segments[0]).unwrap(),
        b"same content"
    );
    assert!(std::fs::read_dir(&directory).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
    let _ = std::fs::remove_dir_all(directory);
}
