// rightkit-agent-recipes 0.1.0 consumed by exact registry pin: deep-link parsing refusals. No I/O.
use rightkit_agent_recipes::parse_recipe_deeplink;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        parse_recipe_deeplink("https://example.com/x").is_err(),
        "foreign URLs are refused",
    );
    check(parse_recipe_deeplink("").is_err(), "empty input is refused");
}
