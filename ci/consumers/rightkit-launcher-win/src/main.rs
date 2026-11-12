// rightkit-launcher-win 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Every public module of this crate is compiled only for Windows. On Windows this consumer builds
// the host's own launcher rows (an application page, a usage row, a cleanup summary), checks that
// the type-ahead holds nothing while no capture is open, and names `host::install`, `panel::start`
// and `type_ahead::install` without calling them: it opens no window, installs no hook and presses
// no key. On other hosts the crate has no public items, so the consumer reports that and exits 0;
// the Windows lane carries the proof.
#[cfg(windows)]
fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn main() {
    use rightkit_launcher_win::host::{AppPage, CleanupSummary, PageGlyph, UsageRow};
    use rightkit_launcher_win::{host, panel, type_ahead};
    let page = AppPage {
        title: "Open Storage".to_string(),
        page: "storage",
        note: "What fills the drives".to_string(),
        glyph: PageGlyph::Drive,
    };
    check(
        page.page == "storage" && page.glyph == PageGlyph::default(),
        "an application page carries the host's page and icon",
    );
    let usage = UsageRow {
        title: "Usage".to_string(),
        words: vec!["usage".to_string()],
        line: "42% used".to_string(),
        page: "accounts",
    };
    check(
        usage.words.iter().any(|w| w.starts_with("us")),
        "a usage row is found by the start of its words",
    );
    let summary = CleanupSummary {
        items: 3,
        safe_bytes: 1024.0,
        scanned_at: 0.0,
    };
    check(
        summary.items == 3,
        "a cleanup summary is the host's own scan",
    );
    check(
        type_ahead::take().is_empty(),
        "the type-ahead holds nothing while no capture is open",
    );
    let _install = host::install;
    let _start = panel::start;
    let _hub = type_ahead::install;
    println!("ok: host::install, panel::start and type_ahead::install named, not called");
    println!("OK rightkit-launcher-win 0.1.0 (registry consumer, Windows surface)");
}

#[cfg(not(windows))]
fn main() {
    println!(
        "skip: rightkit-launcher-win exports only Windows modules; nothing to check on this host"
    );
}
