//! Resumable canonical-corpus runner for the app's exact production L1 path.
//! Candidate input is opaque ASR/L0 output only; scoring data is downstream-only.

use rightkit_dictation_engine::canonical_polish_harness::{
    resolve_cleanup_outcome, select_rows, EvalInputRow,
};
use rightkit_dictation_engine::l3_cleanup::{app_polish_outcome, PolishContext};
use rightkit_dictation_engine::vocabulary;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct OutputRow<'a> {
    id: &'a str,
    duration_s: f64,
    raw_hypothesis: &'a str,
    l0_hypothesis: &'a str,
    hypothesis: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    circuit_open: bool,
    l1_polish_ms: f64,
    product_path: &'static str,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = required_path(&args, "--input")?;
    let out = required_path(&args, "--out")?;
    let min_duration_s = value(&args, "--min-duration")
        .and_then(|v| v.parse().ok())
        .unwrap_or(15.0);
    let pace_ms = value(&args, "--pace-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5_000u64);
    let limit = value(&args, "--limit").and_then(|v| v.parse().ok());
    let wanted: HashSet<String> = repeated_values(&args, "--clip").into_iter().collect();
    let wanted = (!wanted.is_empty()).then_some(&wanted);
    let plan_only = args.iter().any(|arg| arg == "--plan-only");

    let rows = read_input(&input)?;
    let completed = completed_ids(&out)?;
    let eligible = select_rows(&rows, min_duration_s, &HashSet::new(), wanted, limit);
    let pending = select_rows(&rows, min_duration_s, &completed, wanted, limit);
    if plan_only {
        println!(
            "eligible={} completed={} pending={}",
            eligible.len(),
            eligible.len().saturating_sub(pending.len()),
            pending.len()
        );
        for row in &eligible {
            println!("eligible_id={}", row.id);
        }
        return Ok(());
    }
    if pending.is_empty() {
        println!("resume_complete: no provider calls required");
        return Ok(());
    }

    for key in [
        "GROQ_API_KEY",
        "RIGHTKIT_L3_CLEANUP",
        "RIGHTKIT_L3_CLOUD_CONSENT",
    ] {
        if std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_none()
        {
            return Err(format!("{key} is required for the production L1 path"));
        }
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let context = PolishContext {
        app_name: Some("ChatGPT.exe".to_string()),
        window_title: Some("ChatGPT".to_string()),
        field_text: None,
        selected_text: None,
        field_context_available: false,
        vocabulary: vocabulary::terms(),
        writing_region: Some("India".to_string()),
        sound_alikes: vocabulary::sound_alike_pairs(),
    };

    for (index, row) in pending.into_iter().enumerate() {
        if index > 0 && pace_ms > 0 {
            thread::sleep(Duration::from_millis(pace_ms));
        }
        let started = Instant::now();
        let resolved = resolve_cleanup_outcome(
            app_polish_outcome(&row.hypothesis, &context),
            &row.hypothesis,
        );
        let raw = row.raw_hypothesis.as_deref().unwrap_or(&row.hypothesis);
        let output = OutputRow {
            id: &row.id,
            duration_s: row.duration_s,
            raw_hypothesis: raw,
            l0_hypothesis: &row.hypothesis,
            hypothesis: resolved.hypothesis,
            status: resolved.status,
            reason: resolved.reason,
            circuit_open: resolved.circuit_open,
            l1_polish_ms: started.elapsed().as_secs_f64() * 1_000.0,
            product_path: "text_polish::polish_local_only -> l3_cleanup::app_polish_outcome",
        };
        serde_json::to_writer(&mut writer, &output).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_data().map_err(|e| e.to_string())?;
        println!(
            "l1 {} status={} {:.1}ms",
            row.id, output.status, output.l1_polish_ms
        );
    }
    Ok(())
}

fn read_input(path: &Path) -> Result<Vec<EvalInputRow>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    BufReader::new(file)
        .lines()
        .filter(|line| line.as_ref().map(|v| !v.trim().is_empty()).unwrap_or(true))
        .map(|line| {
            let line = line.map_err(|e| e.to_string())?;
            serde_json::from_str(&line).map_err(|e| e.to_string())
        })
        .collect()
}

fn completed_ids(path: &Path) -> Result<HashSet<String>, String> {
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    BufReader::new(file)
        .lines()
        .filter(|line| line.as_ref().map(|v| !v.trim().is_empty()).unwrap_or(true))
        .map(|line| {
            let line = line.map_err(|e| e.to_string())?;
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| e.to_string())?;
            value
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| "output row missing id".to_string())
        })
        .collect()
}

fn required_path(args: &[String], name: &str) -> Result<PathBuf, String> {
    value(args, name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required"))
}

fn value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn repeated_values(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}
