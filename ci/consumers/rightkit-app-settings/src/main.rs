// rightkit-app-settings 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves atomic_write, the public settings-file publication: the first write creates the file with
// the exact bytes, and a second write replaces them whole and reads back. Public API only.
// Does not prove: SettingsBackend, FileBackend, Document, SqlSettings, secret slots, or
// atomic_write_with.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data.
use rightkit_app_settings::atomic_write;
use std::fs;
use std::path::PathBuf;

fn main() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rk-app-settings-consumer-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create consumer temp dir");
    let path = dir.join("settings.json");

    atomic_write(&path, br#"{"theme":"dark"}"#).expect("first publication");
    assert_eq!(
        fs::read(&path).expect("read first publication"),
        br#"{"theme":"dark"}"#.to_vec(),
        "first publication has the exact bytes"
    );

    atomic_write(&path, br#"{"theme":"light"}"#).expect("replacement publication");
    assert_eq!(
        fs::read(&path).expect("read replacement"),
        br#"{"theme":"light"}"#.to_vec(),
        "replacement publication replaces the whole file"
    );

    fs::remove_dir_all(&dir).expect("remove consumer temp dir");
    println!("OK rightkit-app-settings 0.3.0 (registry consumer)");
}
