//! Registry consumer for rightkit-mesh 0.1.1 (`devicelink` feature): builds against the published API only.
use rightkit_mesh::devicelink::circle::circle_pairing_id;
use rightkit_mesh::devicelink::{DeviceIdentity, RegistryStore};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let a = DeviceIdentity::from_seed(
        &[9u8; 32],
        rightkit_mesh::devicelink::DevicePlatform::Linux,
        None,
    )
    .expect("identity");
    let again = DeviceIdentity::from_seed(
        &[9u8; 32],
        rightkit_mesh::devicelink::DevicePlatform::Linux,
        None,
    )
    .expect("identity");
    check(
        a.device_id() == again.device_id() && a.device_id().starts_with("dev_"),
        "a device id derives from the seed",
    );
    let path = std::env::temp_dir().join(format!("rk-mesh-consumer-{}", std::process::id()));
    let store = RegistryStore::new(path.join("registry.json"));
    check(
        store.load().map(|r| r.devices.is_empty()).unwrap_or(false),
        "a missing registry file is an empty registry",
    );
    check(
        circle_pairing_id("dev_a", "dev_b") == circle_pairing_id("dev_b", "dev_a"),
        "circle pairing ids are symmetric",
    );
    let _ = std::fs::remove_dir_all(&path);
    println!("rightkit-mesh devicelink consumer: all checks passed");
}
