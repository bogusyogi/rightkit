// rightkit-desktop-tweaks 0.2.0 consumed by exact registry pin (`=0.2.0`), outside the workspace.
//
// Every public module of this crate (keys, services) is compiled only for Windows. On Windows this
// consumer builds the shortcut settings, checks they start off and that the shortcut handler
// runs after a handler at order 0, and names `keys::apply` and `services::install` without calling
// them, so it installs no hook, presses no key and starts no timer. On other hosts the crate has no public items, so the consumer
// reports that and exits 0 without a check; the Windows lane carries the proof.
#[cfg(windows)]
fn main() {
    use rightkit_desktop_tweaks::{keys, services};
    let off = keys::Shortcuts::default();
    if off.mac || off.save_find_tab_close || off.guards || keys::current() != off {
        eprintln!("FAIL: shortcuts start off");
        std::process::exit(1);
    }
    println!("ok: shortcuts start off");
    if keys::ORDER <= 0 {
        eprintln!("FAIL: the shortcut handler runs after an application's order-0 handler");
        std::process::exit(1);
    }
    println!("ok: the shortcut handler runs after an application's order-0 handler");
    let _apply: fn(keys::Shortcuts) = keys::apply;
    let _install = services::install;
    println!("ok: keys::apply and services::install named, not called");
    println!("OK rightkit-desktop-tweaks 0.2.0 (registry consumer, Windows surface)");
}

#[cfg(not(windows))]
fn main() {
    println!(
        "skip: rightkit-desktop-tweaks exports only Windows modules; nothing to check on this host"
    );
}
