//! Apply the app's production deterministic L0 to a JSONL corpus.

use rightkit_dictation_engine::text_polish;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct InputRow {
    row_id: String,
    input_text: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
struct OutputRow {
    row_id: String,
    source_text: String,
    input_text: String,
    capabilities: Vec<String>,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = required_path(&args, "--input")?;
    let output = required_path(&args, "--output")?;
    let input_file = File::open(&input).map_err(|error| format!("{}: {error}", input.display()))?;
    let output_file =
        File::create(&output).map_err(|error| format!("{}: {error}", output.display()))?;
    let mut writer = BufWriter::new(output_file);
    let mut count = 0usize;

    for line in BufReader::new(input_file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let row: InputRow = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        let capitalize_start = !row.capabilities.iter().any(|value| value == "CONTINUATION");
        let polished = text_polish::polish_local_only_with(&row.input_text, capitalize_start);
        let output_row = OutputRow {
            row_id: row.row_id,
            source_text: row.input_text,
            input_text: polished,
            capabilities: row.capabilities,
        };
        serde_json::to_writer(&mut writer, &output_row).map_err(|error| error.to_string())?;
        writer.write_all(b"\n").map_err(|error| error.to_string())?;
        count += 1;
    }
    writer.flush().map_err(|error| error.to_string())?;
    println!("wrote {count} rows to {}", output.display());
    Ok(())
}

fn required_path(args: &[String], name: &str) -> Result<PathBuf, String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| Path::new(&pair[1]).to_path_buf())
        .ok_or_else(|| format!("{name} is required"))
}
