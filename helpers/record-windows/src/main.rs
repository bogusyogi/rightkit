//! rightkit-record-windows: RK3 Windows recorder helper (first cut).
//!
//! Thin CLI. Capture logic lives in `windows/` and moves as a unit into
//! `crates/rightkit-record/src/windows/` later.

mod args;
mod bmp;
mod codec;
mod error;
mod pacer;
mod record;
mod windows;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let outcome = args::parse(&argv).and_then(|command| match command {
        args::Command::Capture(capture) => windows::run(&capture),
    });
    let code = match outcome {
        Ok(summary) => {
            println!("{summary}");
            0
        }
        Err(err) => {
            println!(
                "{}",
                serde_json::json!({ "ok": false, "code": err.code.as_str(), "message": err.message })
            );
            eprintln!("{err}");
            err.code.exit_code()
        }
    };
    std::process::exit(code);
}
