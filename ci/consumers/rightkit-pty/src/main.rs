// rightkit-pty 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// No pseudo-terminal is opened and no program is started: the crate's own docs list terminal behaviour as
// tested end to end against an installed app, so a consumer lane makes no claim about a live terminal.
// This consumer builds commands and sizes, and drives `spawn` only into the two refusals that happen
// before any terminal or process exists (an invalid size, and no tokio runtime).
use rightkit_pty::{PtyCommand, PtySize, INPUT_CHUNK};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Sizes.
    check("input chunk", INPUT_CHUNK == 8 * 1024);
    let default = PtySize::default();
    check(
        "default size is 24 by 80 with no pixels",
        default == PtySize::new(24, 80) && default.pixel_width == 0 && default.pixel_height == 0,
    );
    let sized = PtySize::new(40, 120).with_pixels(960, 800);
    check(
        "pixel size",
        sized.rows == 40
            && sized.cols == 120
            && sized.pixel_width == 960
            && sized.pixel_height == 800,
    );
    check(
        "sizes compare",
        sized != default && sized == PtySize::new(40, 120).with_pixels(960, 800),
    );

    // Command construction: argv form, and the builder chain.
    check(
        "an empty argv names no program",
        PtyCommand::from_argv(Vec::<String>::new())
            .err()
            .map(|e| e.kind() == std::io::ErrorKind::InvalidInput)
            == Some(true),
    );
    check(
        "a wrapped argv builds",
        PtyCommand::from_argv(["sandbox-exec", "-p", "profile", "--", "program", "arg"]).is_ok(),
    );
    let mut command = PtyCommand::new("a-program-that-is-never-started");
    command
        .arg("one")
        .args(["two", "three"])
        .env("RIGHTKIT_CONSUMER", "1")
        .env_remove("RIGHTKIT_CONSUMER_UNSET")
        .current_dir(std::env::temp_dir())
        .size(sized);
    let _owned = command.owned_mut();
    println!("ok: builder chain accepted every option");

    // An invalid size is refused before a terminal is opened.
    let mut zero_rows = PtyCommand::new("a-program-that-is-never-started");
    zero_rows.size(PtySize::new(0, 80));
    check(
        "zero rows refused",
        zero_rows
            .spawn()
            .err()
            .map(|e| e.kind() == std::io::ErrorKind::InvalidInput)
            == Some(true),
    );
    let mut zero_cols = PtyCommand::new("a-program-that-is-never-started");
    zero_cols.size(PtySize::new(24, 0));
    check(
        "zero columns refused",
        zero_cols
            .spawn()
            .err()
            .map(|e| e.kind() == std::io::ErrorKind::InvalidInput)
            == Some(true),
    );

    // A valid size without a tokio runtime is refused before a terminal is opened.
    check(
        "spawn outside a runtime is refused",
        command.spawn().is_err(),
    );

    println!("OK rightkit-pty 0.1.0 (registry consumer, public API)");
}
