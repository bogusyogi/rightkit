// rightkit-notch-ui 0.2.0 consumed by exact registry pin (`=0.2.0`), outside the workspace.
//
// Proves, through the public API only, the cross-platform layout and motion values:
// - the cells are the host's: before any are set the notch counts one place; the host's cells
//   come back in its order with its glyph and section; the anchor cell is the one a minimised
//   notch keeps, and without an anchor the notch does not minimise;
// - motion::Spring rests at the value it was placed at, and a step toward a new target moves
//   toward it;
// - motion::EMPHASIS_SCALE is the hover growth of 1.12.
// The window, card, blur, render and swell modules are Windows-only and are not used here.
// No window, no timer, no host install, no input. No I/O or network. Exits 1 on the first failed check.
use rightkit_notch_ui::layout::{self, Cell};
use rightkit_notch_ui::motion::{Spring, EMPHASIS, EMPHASIS_SCALE};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(layout::count() == 1, "before the host sets cells the notch counts one place");
    const GAUGE: Cell = Cell::new(7, "cpu", "monitor");
    const SHARE: Cell = Cell::new(9, "Snd", "general");
    const TOOLS: Cell = Cell::new(11, "tools", "settings");
    layout::set_shown(vec![GAUGE, SHARE, TOOLS]);
    check(layout::count() == 3, "the notch draws the host's three cells");
    check(
        layout::cell_at(0) == Some(GAUGE) && layout::cell_at(2).map(Cell::section) == Some("settings"),
        "cells come back in the host's order with its section",
    );
    check(
        layout::cell_at(1).map(Cell::glyph) == Some("Snd"),
        "a cell carries the host's glyph key",
    );
    layout::set_minimised(true);
    check(!layout::minimised(), "without an anchor the notch does not minimise");
    layout::set_anchor(Some(SHARE));
    check(
        layout::minimised() && layout::shown() == vec![SHARE] && layout::count() == 1,
        "minimised, the notch keeps only the anchor cell",
    );
    layout::set_minimised(false);
    check(layout::anchor_cell() == Some(1), "the anchor is the second shown cell");
    layout::set_shown(Vec::new());
    check(layout::count() == 3, "an empty cell list is ignored");
    check(
        Spring::at(10.0).resting_at(10.0),
        "a spring placed at its target is at rest",
    );
    let mut spring = Spring::at(0.0);
    spring.step(10.0, EMPHASIS, 0.5);
    check(
        spring.value > 0.0,
        "a step toward a higher target moves the spring up",
    );
    check(EMPHASIS_SCALE == 1.12, "the hover emphasis scale is 1.12");
    println!("OK rightkit-notch-ui 0.2.0 (registry consumer)");
}
