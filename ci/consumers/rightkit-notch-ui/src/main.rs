//! Registry consumer for rightkit-notch-ui 0.1.0: published API only, no windows opened.
use rightkit_notch_ui::layout::{Edge, scale};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        Edge::parse("left") == Some(Edge::Left) && Edge::Left.is_vertical(),
        "edges parse and side edges are vertical",
    );
    check(Edge::parse("middle").is_none(), "an unknown edge is refused");
    check(scale(96) > 0.0, "the scale at 96 dpi is positive");
}
