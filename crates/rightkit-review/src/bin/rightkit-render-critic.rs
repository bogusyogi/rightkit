//! Out-of-process deterministic critic. Reads a `CriticInput` as JSON on stdin and
//! writes the `Vec<Finding>` as JSON on stdout. Exit 2 on unreadable input.

use std::io::Read;
use std::process::ExitCode;

use rightkit_review::render_qa::critic::{judge, CriticInput};

fn main() -> ExitCode {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("rightkit-render-critic: cannot read stdin: {e}");
        return ExitCode::from(2);
    }
    let input: CriticInput = match serde_json::from_str(&raw) {
        Ok(input) => input,
        Err(e) => {
            eprintln!("rightkit-render-critic: invalid input: {e}");
            return ExitCode::from(2);
        }
    };
    match serde_json::to_string(&judge(&input)) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rightkit-render-critic: cannot encode findings: {e}");
            ExitCode::from(2)
        }
    }
}
