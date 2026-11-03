// rightkit-i18n 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Catalogs are in-memory JSON strings; no file, network or process is touched.
use rightkit_i18n::{merge_catalog, normalize_tag, Catalog, I18n, I18nError, LocaleInfo};
use serde_json::{json, Value};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn catalog(value: Value) -> Catalog {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("test catalogs are objects"),
    }
}

fn main() {
    check(
        "tag normalisation",
        normalize_tag("  zh_Hant-TW ") == "zh-hant-tw",
    );

    let english = catalog(json!({
        "greeting": "Hello",
        "nested": { "a": "A", "b": "B" },
        "has.dot": "dotted"
    }));
    let locales = [
        LocaleInfo {
            id: "fr".into(),
            name: "Fran\u{e7}ais".into(),
            english_name: "French".into(),
            aliases: vec![],
        },
        LocaleInfo {
            id: "zh".into(),
            name: "\u{4e2d}\u{6587}".into(),
            english_name: "Chinese".into(),
            aliases: vec![],
        },
        LocaleInfo {
            id: "zh-hant".into(),
            name: "\u{7e41}\u{9ad4}\u{4e2d}\u{6587}".into(),
            english_name: "Traditional Chinese".into(),
            aliases: vec!["zh-TW".into()],
        },
    ];
    let mut i18n = I18n::new(english.clone(), "en", Some(&locales[..]));
    check("default locale", i18n.default_locale() == "en");
    check("default locale is loaded", i18n.is_loaded("en"));
    check("fr is not loaded yet", !i18n.is_loaded("fr"));

    // Locale tag resolution: exact id, alias, then longest prefix.
    check(
        "exact id",
        i18n.normalize_locale("FR").as_deref() == Some("fr"),
    );
    check(
        "alias resolves",
        i18n.normalize_locale("zh_TW").as_deref() == Some("zh-hant"),
    );
    check(
        "prefix resolves",
        i18n.normalize_locale("zh-CN").as_deref() == Some("zh"),
    );
    check(
        "region prefix resolves",
        i18n.normalize_locale("fr-CA").as_deref() == Some("fr"),
    );
    check("unknown tag is None", i18n.normalize_locale("xx").is_none());

    // Loading refuses what is not a JSON object.
    check(
        "invalid JSON refused",
        matches!(i18n.load_locale("fr", "{"), Err(I18nError::Parse(_))),
    );
    check(
        "non-object refused",
        i18n.load_locale("fr", "[]") == Err(I18nError::NotAnObject),
    );
    i18n.load_locale("fr", r#"{"greeting":"Bonjour","nested":{"a":"A-fr"}}"#)
        .expect("french catalog loads");
    check("fr is loaded", i18n.is_loaded("fr"));

    // Lookup order: locale, English, caller fallback, key.
    check(
        "locale message",
        i18n.t("fr", "greeting", None) == "Bonjour",
    );
    check(
        "dotted path in locale",
        i18n.t("fr", "nested.a", None) == "A-fr",
    );
    check("English fills a gap", i18n.t("fr", "nested.b", None) == "B");
    check(
        "literal dotted key",
        i18n.t("fr", "has.dot", None) == "dotted",
    );
    check(
        "unloaded locale uses English",
        i18n.t("de", "greeting", None) == "Hello",
    );
    check("fallback", i18n.t("fr", "missing", Some("fb")) == "fb");
    check(
        "key is last resort",
        i18n.t("fr", "missing", None) == "missing",
    );

    // Merge over English.
    let merged = i18n.catalog("fr");
    check(
        "merged catalog keeps English leaves",
        merged["nested"]["a"] == "A-fr" && merged["nested"]["b"] == "B",
    );
    check("unloaded catalog is English", i18n.catalog("de") == english);
    check(
        "non-object override leaves base unchanged",
        merge_catalog(&english, &json!(5)) == english,
    );
    let over = merge_catalog(&english, &json!({ "nested": { "b": "B2" }, "extra": "E" }));
    check(
        "object override merges recursively",
        over["nested"]["a"] == "A" && over["nested"]["b"] == "B2" && over["extra"] == "E",
    );

    // Registering a tag makes it resolvable.
    i18n.register_locale("es");
    check(
        "registered tag resolves",
        i18n.normalize_locale("es-MX").as_deref() == Some("es"),
    );

    println!("OK rightkit-i18n 0.1.0 (registry consumer, public API)");
}
