// rightkit-agent-coding-tools 0.1.0 (the `coding_tools` module of rightkit-agent-tools until 0.3.5)
// consumed by exact registry pin: the edit helper the file-editing tools share. No files are
// written and nothing is started.
use rightkit_agent_coding_tools::edit_helpers::apply_single_edit;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let text = "alpha\nbeta\nalpha\n";
    check(
        apply_single_edit(text, "beta", "gamma", false, "consumer").as_deref()
            == Ok("alpha\ngamma\nalpha\n"),
        "a unique match is replaced once",
    );
    check(
        apply_single_edit(text, "alpha", "delta", false, "consumer").is_err(),
        "an ambiguous match is refused, not guessed",
    );
    check(
        apply_single_edit(text, "alpha", "delta", true, "consumer").as_deref()
            == Ok("delta\nbeta\ndelta\n"),
        "replace_all replaces every match",
    );
    check(
        apply_single_edit(text, "omega", "delta", false, "consumer").is_err(),
        "a missing match is an error",
    );
}
