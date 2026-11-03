#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    rightkit_dictation_runtime::entry::run()
}
