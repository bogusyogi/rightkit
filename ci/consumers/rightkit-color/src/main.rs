// rightkit-color 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the planning surface a host uses before any FFmpeg run: the shipped presets parse and are
// unique, an empty plan on Rec.709 is a format-and-tag-only filter with no files, and a malformed
// .cube LUT is refused.
// Does not prove: running the filters (needs an editing FFmpeg), scopes or sampling of real media.
// State: a per-process directory under the OS temp directory, removed at the end.
use rightkit_color::{
    build_fragment, preset, presets, BuildEnv, ColorPlan, ColorSpaceKind, Cube3d,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let all = presets();
    let mut ids: Vec<_> = all.iter().map(|p| p.id.clone()).collect();
    ids.sort();
    ids.dedup();
    check(
        !all.is_empty() && ids.len() == all.len(),
        "the shipped presets parse and have unique ids",
    );
    check(preset("nope").is_none(), "an unknown preset is none");
    let dir = std::env::temp_dir().join(format!("rk-color-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let fragment = build_fragment(
        &ColorPlan::default(),
        ColorSpaceKind::Rec709,
        &BuildEnv::new(&dir),
    );
    check(
        matches!(&fragment, Ok(f) if f.filter.starts_with("format=yuv420p") && f.files.is_empty()),
        "an empty plan on Rec.709 only sets format and tags",
    );
    check(
        Cube3d::parse("not a lut").is_err(),
        "a malformed .cube LUT is refused",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-color consumer: all checks passed");
}
