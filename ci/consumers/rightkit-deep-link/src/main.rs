// rightkit-deep-link 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Parses and queues link strings only. `register_scheme` writes OS state (Windows registry), so it is
// never called here; no network, file or process is touched.
use rightkit_deep_link::deep_link::{parse, DeepLink, DeepLinkQueue};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    let schemes = vec!["myapp".to_string()];

    // A full link: scheme folded to lower case, percent and plus decoding, fragment dropped.
    let link: DeepLink =
        parse("MyApp://open/notes/a%20b?x=1&y=2+3#frag", &schemes).expect("valid link parses");
    check("scheme is lower case", link.scheme == "myapp");
    check("host", link.host == "open");
    check(
        "segments are percent-decoded",
        link.segments == vec!["notes".to_string(), "a b".to_string()],
    );
    check(
        "query is decoded",
        link.query.get("x").map(String::as_str) == Some("1")
            && link.query.get("y").map(String::as_str) == Some("2 3"),
    );
    let wire = serde_json::to_string(&link).expect("link serialises");
    check("camelCase wire has segments", wire.contains("\"segments\""));

    // UTF-8 percent bytes decode as one character, not as bytes-as-chars.
    let utf8 = parse("myapp://open/caf%C3%A9", &schemes).expect("utf-8 link parses");
    check(
        "utf-8 percent decoding",
        utf8.segments == vec!["caf\u{e9}".to_string()],
    );

    // Refusals.
    check(
        "unregistered scheme refused",
        parse("other://open/x", &schemes).is_none(),
    );
    check(
        "traversal refused",
        parse("myapp://open/../etc", &schemes).is_none(),
    );
    check(
        "dot segment refused",
        parse("myapp://open/./x", &schemes).is_none(),
    );
    check(
        "empty host refused",
        parse("myapp:///x", &schemes).is_none(),
    );
    check(
        "encoded control refused",
        parse("myapp://open/a%00b", &schemes).is_none(),
    );
    check(
        "missing scheme separator refused",
        parse("myapp:open", &schemes).is_none(),
    );
    let long = format!("myapp://open/{}", "a".repeat(5000));
    check("over-long input refused", parse(&long, &schemes).is_none());

    // The queue keeps accepted links in arrival order and drains once.
    let queue = DeepLinkQueue::default();
    let count = queue.push_all(
        ["myapp://first/1", "bogus://skip/2", "myapp://second/3"],
        &schemes,
    );
    check("queue counts only accepted links", count == 2);
    let drained = queue.drain();
    check(
        "queue drains in arrival order",
        drained.len() == 2 && drained[0].host == "first" && drained[1].host == "second",
    );
    check("queue is empty after drain", queue.drain().is_empty());

    println!("OK rightkit-deep-link 0.1.0 (registry consumer, public API)");
}
