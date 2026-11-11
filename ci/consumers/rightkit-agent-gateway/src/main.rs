//! Registry consumer for rightkit-agent-gateway 0.3.0: builds against the published API only.
use rightkit_agent_gateway::content::{stamped, untrusted};
use rightkit_agent_gateway::gate::{self, Denied, Level, Logging, Method};
use rightkit_agent_gateway::redact::{find_secret, redact};
use rightkit_agent_gateway::Origin;
use serde_json::json;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Read,
    Write,
}

impl Level for Tier {
    const LOWEST: Self = Tier::Read;
    fn as_str(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Write => "write",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Tier::Read),
            "write" => Some(Tier::Write),
            _ => None,
        }
    }
}

const TABLE: &[Method<Tier, u8>] = &[
    Method {
        name: "list",
        min: Tier::Read,
        scoped: false,
        changes: false,
        kind: 0,
    },
    Method {
        name: "put",
        min: Tier::Write,
        scoped: true,
        changes: true,
        kind: 1,
    },
];

fn main() {
    check(
        gate::lookup(TABLE, "nope").err() == Some(Denied::UnknownMethod),
        "an unknown method is refused",
    );
    let put = gate::lookup(TABLE, "put").expect("put is in the table");
    check(
        gate::allow_level(Tier::Read, put).err().map(Denied::code) == Some("forbidden_profile"),
        "a level below the method's minimum is refused with a stable code",
    );
    check(
        gate::allow_scope(&["a".to_string()], "b").err() == Some(Denied::ScopeNotAllowed)
            && gate::allow_door(true, put.changes).is_err()
            && gate::allow_door(true, false).is_ok(),
        "scope and read-only door checks refuse",
    );
    check(
        gate::conclude::<u8, String>(Ok(1), Err("disk".into()), Logging::Required, |e| e).is_err()
            && gate::conclude::<u8, String>(Ok(1), Err("disk".into()), Logging::BestEffort, |e| e)
                == Ok(1),
        "no audit row, no result, unless the row is best effort",
    );
    check(
        Origin::Gateway.as_str() == "gateway" && Origin::default() == Origin::Local,
        "doors are named and local by default",
    );
    check(
        untrusted("web", "hello", 3)
            == json!({"untrusted": true, "source": "web", "text": "hel", "truncated": true}),
        "third-party text is wrapped and capped",
    );
    check(
        stamped("[unverified]", "hi") == "[unverified]\n\nhi",
        "relayed text carries its stamp first",
    );
    let token = format!("gh{}{}", "p_", "c".repeat(36));
    check(
        redact(&format!("x {token} y")) == ("x [redacted] y".to_string(), 1),
        "a token assembled at run time is masked",
    );
    let planted = format!("{}{}", "Bear", "er abc");
    check(
        find_secret(&json!({ "a": planted }), &[]).is_some(),
        "the leak check finds a planted token",
    );
    println!("rightkit-agent-gateway consumer: all checks passed");
}
