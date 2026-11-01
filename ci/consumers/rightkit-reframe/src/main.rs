// rightkit-reframe 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the contract a host relies on before any model or media is touched: the face-detector
// model is pinned (id, file, size, SHA-256, an https URL at a fixed commit) and is an on-demand
// add-on, and an unknown model root resolves to nothing without downloading.
// Does not prove: detection, tracking, planning or rendering (they need ONNX Runtime, the model
// download and an editing FFmpeg).
use rightkit_reframe::models::{
    resolve_yunet, yunet_spec, YUNET_FILE, YUNET_SHA256, YUNET_SIZE, YUNET_URL,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let spec = yunet_spec();
    check(
        spec.files.len() == 1
            && spec.files[0].path == YUNET_FILE
            && spec.files[0].size == Some(YUNET_SIZE),
        "the model file is pinned by name and size",
    );
    check(
        YUNET_SHA256.len() == 64 && spec.files[0].sha256 == Some(YUNET_SHA256),
        "the model file is pinned by SHA-256",
    );
    check(
        YUNET_URL.starts_with("https://") && YUNET_URL.contains("/raw/"),
        "the model comes from a fixed-commit https URL",
    );
    check(!spec.entitlement, "the model needs no entitlement");
    let root = std::env::temp_dir().join(format!("rk-reframe-consumer-{}", std::process::id()));
    check(
        resolve_yunet(&root).is_none(),
        "an empty model root resolves to nothing, with no download",
    );
    println!("rightkit-reframe consumer: all checks passed");
}
