// rightkit-media-gen 0.1.2 consumed by exact registry pin (default features), outside the workspace.
// Cheap checks on the public surface; no provider keys, no network, no models.
use std::collections::BTreeSet;

use rightkit_media_gen::{credentials, gallery, host, providers, Context};

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

    // The gallery index (0.1.2): one image with a caller sidecar, in a per-process folder.
    let shots =
        std::env::temp_dir().join(format!("rightkit-media-gen-gallery-{}", std::process::id()));
    let project = shots.join("launch");
    std::fs::create_dir_all(&project).expect("gallery folder");
    let image = project.join("hero.png");
    std::fs::write(&image, b"not really a png").expect("image file");
    let mut caller_name = image.file_name().unwrap().to_os_string();
    caller_name.push(host::CALLER_SIDECAR_SUFFIX);
    std::fs::write(
        image.with_file_name(caller_name),
        br#"{"caller":"my-app","label":"My App"}"#,
    )
    .expect("caller sidecar");
    let roots = vec![shots.to_string_lossy().into_owned()];
    let listing = gallery::index_roots(
        &roots,
        None,
        None,
        None,
        false,
        &gallery::IndexFilter::default(),
    )
    .expect("index the gallery");
    check(
        listing.report.total_available == 1 && listing.artifacts.len() == 1,
        "the gallery lists the one image",
    );
    let art = &listing.artifacts[0];
    check(
        art["project_slug"] == "launch" && art["caller"] == "my-app",
        "project from the folder, caller from the sidecar",
    );
    check(
        art["asset_facts"] == host::asset_facts::AssetFacts::unknown().to_value(),
        "an asset without facts reads as unknown",
    );
    let filtered = gallery::index_roots(
        &roots,
        None,
        None,
        None,
        false,
        &gallery::IndexFilter {
            caller: Some("other".into()),
            ..Default::default()
        },
    )
    .expect("filtered index");
    check(
        filtered.report.total_available == 0,
        "the caller filter narrows the listing",
    );
    check(
        host::approval::eyes_file_hash(&image)
            .is_some_and(|h| h.starts_with("sha256:") && h.len() == 23),
        "the eyes hash is sha256: plus 16 hex digits",
    );
    check(
        gallery::IndexFilter {
            eyes: Some("maybe".into()),
            ..Default::default()
        }
        .validate()
        .is_err(),
        "an eyes state the index never reports is refused",
    );
    let _ = std::fs::remove_dir_all(&shots);
}
