// rightkit-desktop-tweaks 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Every public module of this crate (keys, services) is compiled only for Windows. On Windows this
// consumer names both modules and the services install function, which proves the reviewed
// public surface resolves at the pinned exact version. It installs nothing, presses no key and
// starts no screenshot or timer. On other hosts the crate has no public items, so the consumer
// reports that and exits 0 without a check; the Windows lane carries the proof.
#[cfg(windows)]
fn main() {
    #[allow(unused_imports)]
    use rightkit_desktop_tweaks::{keys, services};
    let _install = services::install;
    println!("ok: keys and services resolve; services::install named, not called");
    println!("OK rightkit-desktop-tweaks 0.1.0 (registry consumer, Windows surface)");
}

#[cfg(not(windows))]
fn main() {
    println!(
        "skip: rightkit-desktop-tweaks exports only Windows modules; nothing to check on this host"
    );
}
