//! `ResourceAuthority::unlimited()` through the public API: it admits everything, the finite
//! profiles still deny, and no profile string can select it.

use rightkit_sandbox::{ResourceAuthority, ResourceKind, ResourceProfile};

#[test]
fn unlimited_admits_far_beyond_every_finite_profile() {
    let authority = ResourceAuthority::unlimited();
    assert_eq!(authority.profile(), ResourceProfile::Unlimited);

    let mut held = Vec::new();
    for _ in 0..200 {
        held.push(authority.reserve(ResourceKind::ChildProcess).unwrap());
    }
    for _ in 0..200 {
        held.push(authority.reserve(ResourceKind::BackgroundProcess).unwrap());
    }
    assert_eq!(held.len(), 400);
    drop(held);

    assert!(authority.reserve(ResourceKind::ChildProcess).is_ok());
    assert!(authority.reserve(ResourceKind::BackgroundProcess).is_ok());
}

#[test]
fn practical_still_denies_the_control() {
    let authority = ResourceAuthority::new(ResourceProfile::Practical);
    let _held = authority.reserve(ResourceKind::ChildProcess).unwrap();
    match authority.reserve(ResourceKind::ChildProcess) {
        Err(denied) => assert_eq!(denied.code(), "resource_admission_denied"),
        Ok(_) => panic!("Practical must deny a second reservation"),
    }
}

#[test]
fn a_profile_string_cannot_select_unlimited() {
    match ResourceProfile::parse("unlimited") {
        Err(message) => assert!(
            message.contains("unknown resource profile"),
            "unexpected message: {message}"
        ),
        Ok(profile) => panic!("the string unlimited must not parse, got {profile:?}"),
    }
    assert_eq!(ResourceProfile::default(), ResourceProfile::Practical);
}

#[test]
fn clones_share_one_count() {
    let a = ResourceAuthority::unlimited();
    let b = a.clone();

    let from_a = a.reserve(ResourceKind::ChildProcess).unwrap();
    let from_b = b.reserve(ResourceKind::ChildProcess).unwrap();
    drop(from_a);
    drop(from_b);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut held = Vec::new();
            for _ in 0..1000 {
                held.push(a.reserve(ResourceKind::ChildProcess).unwrap());
            }
        });
        scope.spawn(|| {
            let mut held = Vec::new();
            for _ in 0..1000 {
                held.push(b.reserve(ResourceKind::ChildProcess).unwrap());
            }
        });
    });

    assert!(a.reserve(ResourceKind::ChildProcess).is_ok());
}

#[cfg(unix)]
#[test]
fn unlimited_sandbox_spawns_beyond_the_practical_child_cap() {
    use rightkit_sandbox::{Sandbox, SandboxCommand, SandboxError, SandboxPolicy};

    let sandbox = Sandbox::new(ResourceAuthority::unlimited());
    let policy = SandboxPolicy::new();
    let mut children = Vec::new();
    for _ in 0..6 {
        let child = sandbox
            .spawn(&policy, SandboxCommand::new("sleep").arg("1"))
            .expect("an unlimited authority admits every child");
        children.push(child);
    }
    assert_eq!(children.len(), 6);
    for mut child in children {
        child.wait().unwrap();
    }

    let practical = Sandbox::new(ResourceAuthority::new(ResourceProfile::Practical));
    let mut held = Vec::new();
    let mut denied = 0;
    for _ in 0..6 {
        match practical.spawn(&policy, SandboxCommand::new("sleep").arg("1")) {
            Ok(child) => held.push(child),
            Err(SandboxError::ResourceDenied(_)) => denied += 1,
            Err(other) => panic!("unexpected spawn error: {other}"),
        }
    }
    assert!(
        denied >= 1,
        "Practical must deny at least one of six children"
    );
    for mut child in held {
        child.wait().unwrap();
    }
}
