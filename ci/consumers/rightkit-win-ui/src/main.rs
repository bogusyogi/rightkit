// rightkit-win-ui 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Every public module of this crate is compiled only for Windows. On Windows this consumer names
// each public module, which proves the reviewed surface resolves at the pinned exact version. It
// creates no window, surface, canvas, font, process or file. On other hosts the crate has no public
// items, so the consumer reports that and exits 0 without a check; the Windows lane carries the proof.
#[cfg(windows)]
fn main() {
    #[allow(unused_imports)]
    use rightkit_win_ui::{
        canvas, clock, diag, fileicon, glyphs, process, raii, surface, text_layout,
    };
    println!("ok: nine Windows modules resolve; none constructed");
    println!("OK rightkit-win-ui 0.1.0 (registry consumer, Windows surface)");
}

#[cfg(not(windows))]
fn main() {
    println!("skip: rightkit-win-ui exports only Windows modules; nothing to check on this host");
}
