// rightkit-doctor 0.1.0 consumed by exact registry pin (default features), outside the workspace.
use rightkit_doctor::command::{run, DoctorHost};
use rightkit_doctor::report::Identity;
use rightkit_doctor::row::{readiness, Row};
use rightkit_doctor::verdict::{Check, Status, VerdictReport};
use serde_json::Value;

struct Host;

impl DoctorHost for Host {
    fn identity(&self) -> Identity {
        Identity::new("Consumer", "consumer-engine", "0.0.1")
    }
    fn checks(&self, _input: &Value) -> Vec<Row> {
        vec![Row::ok("data_dir", true, "writable")]
    }
}

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let report = VerdictReport::new(vec![
        Check::pass("a", "A", "fine"),
        Check::new("b", "B", Status::Block, "gone", "install it"),
    ]);
    check(
        report.exit_code() == 2 && report.render_text("Consumer").contains("install it"),
        "verdict and exit code",
    );
    let rows = vec![
        Row::ok("a", true, "fine"),
        Row::unavailable("b", false, "none"),
    ];
    check(
        readiness(&rows)["status"] == "ready",
        "optional rows do not block",
    );
    let value = run(&Host, &serde_json::json!({}), false);
    check(
        value["engine"] == "consumer-engine" && value["capabilities"][0]["name"] == "doctor",
        "engine report envelope",
    );
    check(
        rightkit_doctor::clock::rfc3339_millis(std::time::UNIX_EPOCH) == "1970-01-01T00:00:00.000Z",
        "epoch formatting",
    );
}
