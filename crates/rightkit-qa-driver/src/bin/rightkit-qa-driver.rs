//! rightkit-qa-driver CLI (macOS). Every command targets a pid and never
//! activates the app. Every effectful command is admitted through an
//! EFF-001 gate (`--policy FILE` / `RIGHTKIT_CONTROL_POLICY`; default
//! allow-all, still settled). See `rightkit_qa_driver::cli`.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "rightkit-qa-driver: this build supports macOS only (Windows backend: see design.md)"
    );
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    use rightkit_qa_driver::cli;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cfg, rest) = match cli::parse_global(&args, |k| std::env::var(k).ok()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };
    let gate = match cli::gate_for(&cfg) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    std::process::exit(cli::run(&rest, gate));
}
