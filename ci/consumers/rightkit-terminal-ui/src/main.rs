// rightkit-terminal-ui 0.1.0 consumed by exact registry pin: command-effect resolution, notification
// escape sequences with a host app name, history path and composer text. No terminal is opened.
use rightkit_terminal_ui::command_effects::CommandEffect;
use rightkit_terminal_ui::composer::{default_history_path, Composer};
use rightkit_terminal_ui::terminal::notify::{set_app_name, title_sequence};

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
        CommandEffect::resolve("clear") == Some(CommandEffect::New),
        "/clear resolves to New",
    );
    check(
        CommandEffect::resolve("quit").is_none(),
        "/quit is not a command effect",
    );
    set_app_name("Consumer");
    check(
        title_sequence("repo", false) == "\x1b]0;Consumer · repo\x07",
        "title carries the host app name",
    );
    let root = std::env::temp_dir();
    check(
        default_history_path(&root) == root.join("history.jsonl"),
        "history path under the state root",
    );
    let mut composer = Composer::new();
    composer.set_text("hello");
    check(!composer.is_empty(), "composer holds typed text");
}
