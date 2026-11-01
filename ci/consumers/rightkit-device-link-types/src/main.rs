//! Registry consumer for rightkit-device-link-types 0.1.0: builds against the published API only.
use rightkit_device_link_types::{
    DevicePlatform, DeviceRef, EffectClass, GuestPass, MemberRole, Registry, TokenGrant,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        EffectClass::Spend.is_irreversible() && !EffectClass::Reversible.is_irreversible(),
        "effect classes tell reversible from irreversible",
    );
    let device = DeviceRef {
        schema_version: 1,
        device_id: "dev_a".to_string(),
        platform: DevicePlatform::Mac,
        display_name: None,
    };
    let text = serde_json::to_string(&device).expect("serialize");
    check(
        text.contains("\"mac\""),
        "platforms serialize in kebab case",
    );
    let registry = Registry::default();
    check(
        registry.get("dev_a").is_none() && registry.devices.is_empty(),
        "a default registry is empty",
    );
    check(
        MemberRole::default() == MemberRole::Unclassified && MemberRole::Server.needs_phone(),
        "an unclassified member needs the phone for reserved actions",
    );
    check(
        GuestPass::check_code("p", "k") == GuestPass::check_code("p", "k"),
        "the guest check code is deterministic",
    );
    check(
        !TokenGrant::new("dev_a", "p", &[1u8; 32], 5).can_admit,
        "a grant never admits",
    );
    println!("rightkit-device-link-types consumer: all checks passed");
}
