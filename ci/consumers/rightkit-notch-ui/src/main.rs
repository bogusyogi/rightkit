// rightkit-notch-ui 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the cross-platform layout and motion values:
// - layout::CELL_COUNT is six cells;
// - motion::Spring rests at the value it was placed at, and a step toward a new target moves
//   toward it;
// - motion::EMPHASIS_SCALE is the hover growth of 1.12.
// The window, card, blur, render and swell modules are Windows-only and are not used here.
// No window, no timer, no host install, no input. No I/O or network. Exits 1 on the first failed check.
use rightkit_notch_ui::layout::CELL_COUNT;
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
    check(CELL_COUNT == 6, "the notch has six cells");
    check(Spring::at(10.0).resting_at(10.0), "a spring placed at its target is at rest");
    let mut spring = Spring::at(0.0);
    spring.step(10.0, EMPHASIS, 0.5);
    check(spring.value > 0.0, "a step toward a higher target moves the spring up");
    check(EMPHASIS_SCALE == 1.12, "the hover emphasis scale is 1.12");
    println!("OK rightkit-notch-ui 0.1.0 (registry consumer)");
}
