//! Standalone CLI: replays the production L1 guardrail chain
//! (`rightkit_dictation_engine::guardrail_replay::replay_guardrails`) over a JSONL
//! file of `{input_text, output_text, field_text?}` rows and reports
//! per-row pass/fail with the failing guard's reason.
//!
//! Usage:
//!   `guardrail_replay --input <path> --output <path> [--strict]`
//!
//! Input JSONL row: {"id"?: string, "input_text": string, "output_text": string, "field_text"?: string}
//! Output JSONL row: {"id": string|null, "pass": bool, "reason": string|null, "accepted_text": string|null}
//! Stdout summary: `pass=<n> fail=<n> total=<n>`
//! Exit code: non-zero only when --strict is passed and at least one row failed.

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::ExitCode;

use rightkit_dictation_engine::guardrail_replay::{replay_guardrails, GuardrailOutcome};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct InputRow {
    #[serde(default)]
    id: Option<String>,
    input_text: String,
    output_text: String,
    #[serde(default)]
    field_text: Option<String>,
}

#[derive(Serialize)]
struct ReportRow {
    id: Option<String>,
    pass: bool,
    reason: Option<String>,
    accepted_text: Option<String>,
}

struct Args {
    input: String,
    output: String,
    strict: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut strict = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = Some(args.next().ok_or("--input requires a path")?),
            "--output" => output = Some(args.next().ok_or("--output requires a path")?),
            "--strict" => strict = true,
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(Args {
        input: input.ok_or("missing --input <path>")?,
        output: output.ok_or("missing --output <path>")?,
        strict,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("guardrail_replay: {err}");
            eprintln!("usage: guardrail_replay --input <path> --output <path> [--strict]");
            return ExitCode::FAILURE;
        }
    };

    let infile = match File::open(&args.input) {
        Ok(f) => f,
        Err(err) => {
            eprintln!(
                "guardrail_replay: failed to open --input {}: {err}",
                args.input
            );
            return ExitCode::FAILURE;
        }
    };
    let outfile = match File::create(&args.output) {
        Ok(f) => f,
        Err(err) => {
            eprintln!(
                "guardrail_replay: failed to create --output {}: {err}",
                args.output
            );
            return ExitCode::FAILURE;
        }
    };
    let mut writer = BufWriter::new(outfile);

    let mut pass: u64 = 0;
    let mut fail: u64 = 0;
    let mut total: u64 = 0;

    for (line_no, line) in BufReader::new(infile).lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(err) => {
                eprintln!(
                    "guardrail_replay: read error at line {}: {err}",
                    line_no + 1
                );
                return ExitCode::FAILURE;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: InputRow = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(err) => {
                eprintln!(
                    "guardrail_replay: malformed JSON at line {}: {err}",
                    line_no + 1
                );
                return ExitCode::FAILURE;
            }
        };
        total += 1;
        let outcome =
            replay_guardrails(&row.input_text, &row.output_text, row.field_text.as_deref());
        let report_row = match outcome {
            GuardrailOutcome::Pass { accepted_text } => {
                pass += 1;
                ReportRow {
                    id: row.id,
                    pass: true,
                    reason: None,
                    accepted_text: Some(accepted_text),
                }
            }
            GuardrailOutcome::Fail { reason } => {
                fail += 1;
                ReportRow {
                    id: row.id,
                    pass: false,
                    reason: Some(reason.to_string()),
                    accepted_text: None,
                }
            }
        };
        let serialized = match serde_json::to_string(&report_row) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("guardrail_replay: serialize error: {err}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(err) = writeln!(writer, "{serialized}") {
            eprintln!("guardrail_replay: write error: {err}");
            return ExitCode::FAILURE;
        }
    }

    if let Err(err) = writer.flush() {
        eprintln!("guardrail_replay: flush error: {err}");
        return ExitCode::FAILURE;
    }

    println!("pass={pass} fail={fail} total={total}");

    if args.strict && fail > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
