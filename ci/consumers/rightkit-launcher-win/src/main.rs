// Registry consumer of rightkit-launcher-win 0.2.0.

#[cfg(windows)]
fn main() {
    use rightkit_launcher_win::{chat, host, snippets, store, type_ahead};

    let _entry_points = (
        host::install,
        chat::load,
        snippets::sync_later,
        store::apps,
        type_ahead::install,
    );
    if !type_ahead::take().is_empty() {
        eprintln!("FAIL: idle type-ahead returned text");
        std::process::exit(1);
    }
    println!("ok: idle type-ahead returned no text");
    println!("OK rightkit-launcher-win 0.2.0 (registry consumer, Windows services)");
}

#[cfg(not(windows))]
fn main() {
    println!("skip: rightkit-launcher-win requires Windows");
}
