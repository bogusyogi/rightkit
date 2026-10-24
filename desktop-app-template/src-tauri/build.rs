fn main() {
    // The engine sidecar (`bundle.externalBin`) is merged in by src-tauri/tauri.sidecar.conf.json for release and dev builds
    // only, so a plain `cargo check`, clippy or test never needs the staged binary and this script reads no environment.
    tauri_build::build()
}
