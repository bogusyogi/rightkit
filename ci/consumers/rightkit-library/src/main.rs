// rightkit-library 0.1.0 consumed by exact registry pin (default features), outside the workspace.
use std::path::Path;

use rightkit_library::artifacts::{self, ArtifactKind, IndexFilter};
use serde_json::json;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        artifacts::classify(Path::new("a.png")) == ArtifactKind::Image,
        "png is an image",
    );
    check(
        artifacts::classify(Path::new("a.mp4")) == ArtifactKind::Video,
        "mp4 is a video",
    );
    let catalog =
        json!({"default_code": "NA", "brands": [{"code": "AB", "segment_prefixes": ["alpha"]}]});
    check(
        artifacts::infer_brand_code_with("/x/alpha-1/p.png", &catalog) == "AB",
        "brand prefix match",
    );
    check(
        artifacts::infer_brand_code("/x/y.png") == "NA",
        "unregistered default",
    );

    let dir =
        std::env::temp_dir().join(format!("rightkit-library-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("one.png"),
        [0x89, b'P', b'N', b'G', 13, 10, 26, 10],
    )
    .expect("write");
    let r = artifacts::index_roots(
        &[dir.to_string_lossy().to_string()],
        None,
        None,
        None,
        false,
        &IndexFilter::default(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    check(
        matches!(&r, Ok(resp) if resp.artifacts.len() == 1),
        "index finds the one image",
    );
}
