// rightkit-theme 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Resolves brand files held in memory into per-mode tokens and design-token JSON: no file, network
// or process is touched.
use rightkit_theme::color::{contrast_ratio, parse_hex, readable_on, to_hex};
use rightkit_theme::{
    design_tokens, design_tokens_json, mode_tokens, parse_brand, resolve_brand, suite_defaults,
    Mode, OVERRIDABLE_TOKENS,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    let brand = parse_brand(
        r##"{
            "name": "Acme",
            "colors": { "brand": "#0A84FF" },
            "dark": { "brand": "#5AC8FF" },
            "tokens": { "light": { "star": "#C09000" } },
            "wordmarkFont": "Inter"
        }"##,
    )
    .unwrap_or_else(|e| {
        eprintln!("FAIL: valid brand refused: {e}");
        std::process::exit(1);
    });
    check("brand name parsed", brand.name == "Acme");

    // Suite defaults per mode and the overridable list.
    check(
        "light plane default",
        suite_defaults(Mode::Light)["plane"] == "#FFFCF5",
    );
    check(
        "dark chrome default",
        suite_defaults(Mode::Dark)["chrome"] == "#0F0F10",
    );
    check(
        "accent-ink is overridable",
        OVERRIDABLE_TOKENS.contains(&"accent-ink"),
    );
    check(
        "mode names",
        Mode::Light.as_str() == "light" && Mode::Dark.as_str() == "dark",
    );

    // Resolution: light accent defaults to the brand colour, the given dark brand is kept.
    let resolved = resolve_brand(&brand).unwrap_or_else(|e| {
        eprintln!("FAIL: resolve refused: {e}");
        std::process::exit(1);
    });
    check(
        "light accent is the brand colour",
        resolved.palette[&Mode::Light]["brand-action"] == "#0A84FF",
    );
    check(
        "given dark brand is kept",
        resolved.palette[&Mode::Dark]["brand"] == "#5AC8FF"
            && resolved.palette[&Mode::Dark]["brand-action"] == "#5AC8FF",
    );
    let accent = parse_hex("#0A84FF").unwrap_or_else(|| std::process::exit(1));
    check(
        "accent-ink is the readable ink on the accent",
        resolved.overrides[&Mode::Light]["accent-ink"] == to_hex(readable_on(accent)),
    );
    check(
        "token override reaches the effective palette",
        resolved.effective[&Mode::Light]["star"] == "#C09000",
    );

    // Flat tokens per mode: brand-mark is the logo colour and accent aliases brand-action.
    let dark = mode_tokens(&resolved, Mode::Dark);
    check(
        "dark tokens",
        dark["brand-mark"] == "#5AC8FF"
            && dark["accent"] == dark["brand-action"]
            && dark.contains_key("plane"),
    );

    // A brand with no dark colours is lifted to a readable contrast on the dark surface.
    let plain = parse_brand(r##"{"name":"Plain","colors":{"brand":"#000000"}}"##)
        .unwrap_or_else(|_| std::process::exit(1));
    let plain_resolved = resolve_brand(&plain).unwrap_or_else(|_| std::process::exit(1));
    let lifted = &plain_resolved.palette[&Mode::Dark]["brand"];
    let lifted_rgb = parse_hex(lifted).unwrap_or_else(|| std::process::exit(1));
    let surface = parse_hex("#1F1F23").unwrap_or_else(|| std::process::exit(1));
    check(
        "dark brand lifted to readable contrast",
        lifted != "#000000" && contrast_ratio(lifted_rgb, surface) >= 4.4,
    );

    // DTCG document: deterministic, per-mode groups, motion scale and the wordmark font.
    let doc = design_tokens(&brand).unwrap_or_else(|_| std::process::exit(1));
    check(
        "design token shape",
        doc["light"]["accent"]["$type"] == "color"
            && doc["dark"]["accent"]["$value"] == "#5AC8FF"
            && doc["motion"]["dur-1"]["$value"] == "120ms"
            && doc["wordmarkFont"]["$value"] == "Inter",
    );
    let text = design_tokens_json(&brand).unwrap_or_else(|_| std::process::exit(1));
    check(
        "json is deterministic and ends with a newline",
        text.ends_with('\n') && text == design_tokens_json(&brand).unwrap_or_default(),
    );

    // Refusals.
    check("array refused", parse_brand("[]").is_err());
    check(
        "missing brand colour refused",
        parse_brand(r#"{"name":"X","colors":{}}"#).is_err(),
    );
    check(
        "non-hex colour refused",
        parse_brand(r#"{"name":"X","colors":{"brand":"blue"}}"#).is_err(),
    );
    check(
        "unknown token override refused",
        parse_brand(
            r##"{"name":"X","colors":{"brand":"#000"},"tokens":{"light":{"nope":"#000"}}}"##,
        )
        .is_err(),
    );
    check(
        "unsafe font refused",
        parse_brand(r##"{"name":"X","colors":{"brand":"#000"},"wordmarkFont":"a;b"}"##).is_err(),
    );

    println!("OK rightkit-theme 0.1.0 (registry consumer, public API)");
}
