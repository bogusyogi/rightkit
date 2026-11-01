//! Registry consumer for rightkit-mail 0.1.1: builds against the published API only.
use rightkit_mail::fusion::{fuse, Hit, RRF_K};
use rightkit_mail::images::trackers::is_tracker_host;
use rightkit_mail::search::query::parse_search;
use rightkit_mail::search::{search_threads_page, SearchRow, SearchStore};
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

struct NoRow;
impl SearchRow for NoRow {
    fn account_id(&self) -> &str {
        ""
    }
    fn thread_id(&self) -> &str {
        ""
    }
    fn matching_message_id(&self) -> Option<&str> {
        None
    }
    fn set_match_count(&mut self, _: i64) {}
    fn set_matching_message(&mut self, _: String, _: Option<String>) {}
    fn set_matching_snippet(&mut self, _: String) {}
}

struct EmptyStore;
impl SearchStore for EmptyStore {
    type Row = NoRow;
    fn now_ms(&self) -> i64 {
        0
    }
    fn query_threads(&self, _: &str, _: &[String]) -> Result<Vec<NoRow>, String> {
        Ok(Vec::new())
    }
    fn query_count(&self, _: &str, _: &[String]) -> Result<i64, String> {
        Ok(0)
    }
    fn query_text_rows(&self, _: &str, _: &[String]) -> Result<Vec<Vec<String>>, String> {
        Ok(Vec::new())
    }
}

fn main() {
    check(
        search_threads_page(&EmptyStore, "invoice", None, None, None, None, 5)
            .is_ok_and(|rows| rows.is_empty()),
        "the planner runs a query against a host store",
    );
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
