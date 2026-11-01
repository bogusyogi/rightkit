// rightkit-media-gen 0.1.0 consumed by exact registry pin (default features), outside the workspace.
// Cheap checks on the public surface; no provider keys, no network, no models.
use std::collections::BTreeSet;

use rightkit_media_gen::{credentials, host, providers, Context};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let reg = providers::registry();
    let ids: BTreeSet<&str> = reg.iter().map(|p| p.id()).collect();
    check(
        !reg.is_empty() && ids.len() == reg.len(),
        "provider ids are unique",
    );
    check(
        ids.iter().all(|id| !id.is_empty()),
        "provider ids are non-empty",
    );

    let root = std::env::temp_dir().join("rightkit-media-gen-consumer");
    let ctx = Context::new(root.clone());
    check(
        ctx.db_path() == root.join("media-gen.sqlite3"),
        "default db file name",
    );
    check(
        ctx.library_dir() == root.join("library"),
        "default library folder",
    );
    let ctx = ctx.with_db_file_name("host.sqlite3");
    check(
        ctx.db_path() == root.join("host.sqlite3"),
        "db file name override",
    );

    let caller = host::parse_caller("My App").expect("valid caller");
    check(caller.slug == "my-app", "caller slug");
    check(
        host::parse_caller("../etc").is_err(),
        "unsafe caller refused",
    );
    check(
        credentials::service() == "dev.rightkit.app",
        "neutral default keychain service",
    );
}
