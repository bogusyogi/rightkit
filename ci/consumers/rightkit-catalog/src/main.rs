// rightkit-catalog 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Catalogs are in-memory JSON strings; the one directory read targets a path that cannot exist.
// Nothing is written and no network or process is used.
use rightkit_catalog::{
    check, generate, load_dir, parse_catalog, placeholders, Catalogs, Finding, Target,
    REFERENCE_LOCALE,
};

fn fail(name: &str) -> ! {
    eprintln!("FAIL: {name}");
    std::process::exit(1);
}

fn check_ok(name: &str, ok: bool) {
    if !ok {
        fail(name);
    }
    println!("ok: {name}");
}

fn main() {
    check_ok("reference locale is en", REFERENCE_LOCALE == "en");

    // Target names.
    check_ok("ts target", Target::parse("ts") == Some(Target::TypeScript));
    check_ok("rust target", Target::parse("rs") == Some(Target::Rust));
    check_ok(
        "swift target",
        Target::parse("swift") == Some(Target::Swift),
    );
    check_ok("unknown target", Target::parse("go").is_none());

    // Flattening: nested objects join with a dot.
    let en = parse_catalog(
        "en.json",
        r#"{"menu":{"open":"Open","close":"Close"},"title":"Hello {name}"}"#,
    )
    .unwrap_or_else(|_| fail("english catalog parses"));
    check_ok(
        "nested keys flatten",
        en.get("menu.open").map(String::as_str) == Some("Open") && en.len() == 3,
    );
    check_ok(
        "non-object top level refused",
        parse_catalog("bad.json", "[]").is_err(),
    );
    check_ok(
        "non-string leaf refused",
        parse_catalog("bad.json", r#"{"n":1}"#).is_err(),
    );
    check_ok(
        "dotted key refused",
        parse_catalog("bad.json", r#"{"a.b":"x"}"#).is_err(),
    );

    // Placeholders: sorted, unique, doubled braces are literal.
    check_ok(
        "placeholders",
        placeholders("{b} and {a} {{literal}} {a}") == vec!["a".to_string(), "b".to_string()],
    );

    // Completeness: missing key, placeholder drift and an extra key.
    let fr = parse_catalog(
        "fr.json",
        r#"{"menu":{"open":"Ouvrir"},"title":"Bonjour","bonus":"x"}"#,
    )
    .unwrap_or_else(|_| fail("french catalog parses"));
    let mut catalogs = Catalogs::new();
    catalogs.insert("en".to_string(), en);
    catalogs.insert("fr".to_string(), fr);
    let findings = check(&catalogs);
    check_ok("three findings", findings.len() == 3);
    check_ok(
        "missing key found",
        findings.contains(&Finding::Missing {
            locale: "fr".into(),
            key: "menu.close".into(),
        }),
    );
    check_ok(
        "placeholder drift found",
        findings.contains(&Finding::Placeholders {
            locale: "fr".into(),
            key: "title".into(),
            english: vec!["name".into()],
            found: vec![],
        }),
    );
    check_ok(
        "extra key found",
        findings.contains(&Finding::Extra {
            locale: "fr".into(),
            key: "bonus".into(),
        }),
    );
    check_ok(
        "finding text",
        Finding::Missing {
            locale: "fr".into(),
            key: "k".into(),
        }
        .to_string()
            == "fr: missing k",
    );

    // Deterministic generation for all three targets.
    let ts = generate(&catalogs, Target::TypeScript);
    check_ok(
        "typescript module",
        ts.contains("export const messages = {") && ts.contains("\"menu.open\": \"Open\""),
    );
    check_ok(
        "typescript output is deterministic",
        ts == generate(&catalogs, Target::TypeScript),
    );
    let rust = generate(&catalogs, Target::Rust);
    check_ok(
        "rust module",
        rust.contains("pub static LOCALES: &[&str] = &[\"en\", \"fr\"];")
            && rust.contains("pub static MESSAGES_EN: Table"),
    );
    let swift = generate(&catalogs, Target::Swift);
    check_ok(
        "swift file",
        swift.contains("public enum Catalog {") && swift.contains("messages_FR"),
    );

    // The reference catalog is required: a path below an executable file can never be a directory.
    let executable = std::env::current_exe().unwrap_or_else(|_| fail("current exe"));
    check_ok(
        "missing directory refused",
        load_dir(&executable.join("rightkit-catalog-missing")).is_err(),
    );

    println!("OK rightkit-catalog 0.1.0 (registry consumer, public API)");
}
