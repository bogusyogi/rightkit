//! Registry consumer for rightkit-mail 0.1.0: builds against the published API only.
use rightkit_mail::fusion::{fuse, Hit, RRF_K};
use rightkit_mail::images::trackers::is_tracker_host;
use rightkit_mail::search::query::parse_search;
use rightkit_mail::trust::parse_auth_results;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn hit(rowid: i64) -> Hit {
    Hit {
        rowid,
        internal_date: 0,
        message_id: format!("m{rowid}"),
    }
}

fn main() {
    let terms = parse_search("from:alice invoice");
    check(
        terms.from.as_deref() == Some("alice"),
        "from: is compiled into the terms",
    );
    let fused = fuse(&[hit(1), hit(2)], &[hit(2), hit(3)], RRF_K);
    check(
        fused.first() == Some(&2),
        "a row both rankings like ranks first",
    );
    check(
        parse_auth_results("mx.example; spf=pass smtp.mailfrom=a.example").spf == "pass",
        "an SPF pass is read from Authentication-Results",
    );
    check(
        !is_tracker_host("example.org"),
        "an ordinary host is not a tracker",
    );
    println!("rightkit-mail consumer: all checks passed");
}
