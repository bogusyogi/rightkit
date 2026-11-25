//! Registry consumer for rightkit-profile 0.1.0: builds against the published API only.
use rightkit_profile::{entry_id, parse_date, Area, NAME};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(NAME == "rightkit-profile", "the package name reads back");
    check(
        entry_id(Area::Work, "role") == "work/role",
        "an entry id is area slug and key",
    );
    check(
        Area::from_slug("work") == Some(Area::Work),
        "an area is found by its slug",
    );
    check(Area::ALL.len() == 10, "the profile has ten areas");
    check(
        parse_date("2026-02-28").is_some() && parse_date("2026-02-30").is_none(),
        "only real calendar dates parse",
    );
    println!("rightkit-profile consumer: all checks passed");
}
