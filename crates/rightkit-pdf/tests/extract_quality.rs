#![cfg(feature = "extract")]

//! Text extraction quality harness: word F1 and reading order against poppler `pdftotext` on a
//! pinned corpus (Orthic-Labs/rightkit#278). Test-only, opt-in, no network.
//!
//! The measuring test is `#[ignore]`: it needs the external oracle. Its method, the corpus pin, the
//! baseline and the decision rule are in `tests/fixtures/extract-quality/README.md`. The scoring
//! functions below have ordinary tests that run without the oracle.
//!
//! A missing oracle is a **failure** (set `RIGHTKIT_PDF_QUALITY_ALLOW_SKIP=1` for a loudly printed
//! skip), never a silent pass. Design reference: pdfcraft `xtask/src/gates.rs` `text_oracle` and
//! `xtask/src/corpus.rs` (MIT OR Apache-2.0; no code is copied).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_pdf::model::sha256_hex;
use serde_json::{json, Value};

/// Scoring: pure functions, no I/O.
mod scoring {
    use std::collections::HashMap;

    /// Words above which a page's reading order is scored on its first part only (the comparison is
    /// quadratic).
    pub const MAX_ORDER_WORDS: usize = 4000;

    /// Lower-cased alphanumeric words.
    pub fn words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    /// Size of the multiset intersection of two word lists.
    pub fn overlap(a: &[String], b: &[String]) -> usize {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for word in a {
            *counts.entry(word.as_str()).or_default() += 1;
        }
        let mut common = 0;
        for word in b {
            if let Some(left) = counts.get_mut(word.as_str()) {
                if *left > 0 {
                    *left -= 1;
                    common += 1;
                }
            }
        }
        common
    }

    /// F1 from an overlap and the two list sizes. Two empty lists agree perfectly.
    pub fn f1(overlap: usize, extracted: usize, oracle: usize) -> f64 {
        if extracted == 0 && oracle == 0 {
            return 1.0;
        }
        if overlap == 0 {
            return 0.0;
        }
        let precision = overlap as f64 / extracted as f64;
        let recall = overlap as f64 / oracle as f64;
        2.0 * precision * recall / (precision + recall)
    }

    /// Length of the longest common subsequence of two word lists.
    pub fn lcs<'a>(a: &'a [String], b: &'a [String]) -> usize {
        let mut table: HashMap<&'a str, u32> = HashMap::new();
        let mut id_of = |word: &'a String| -> u32 {
            let next = table.len() as u32;
            *table.entry(word.as_str()).or_insert(next)
        };
        let a: Vec<u32> = a.iter().map(&mut id_of).collect();
        let b: Vec<u32> = b.iter().map(&mut id_of).collect();
        let mut previous = vec![0usize; b.len() + 1];
        let mut current = vec![0usize; b.len() + 1];
        for x in &a {
            for (j, y) in b.iter().enumerate() {
                current[j + 1] = if x == y {
                    previous[j] + 1
                } else {
                    previous[j + 1].max(current[j])
                };
            }
            std::mem::swap(&mut previous, &mut current);
        }
        previous[b.len()]
    }

    /// Reading-order score of one page: the share of the words both sides have that come in the same
    /// order (LCS over overlap). `None` when they share no word.
    pub fn order_score(extracted: &[String], oracle: &[String]) -> Option<(f64, usize)> {
        let a = &extracted[..extracted.len().min(MAX_ORDER_WORDS)];
        let b = &oracle[..oracle.len().min(MAX_ORDER_WORDS)];
        let shared = overlap(a, b);
        if shared == 0 {
            return None;
        }
        Some((lcs(a, b) as f64 / shared as f64, shared))
    }
}

#[cfg(test)]
mod scoring_tests {
    use super::scoring::*;

    fn w(text: &str) -> Vec<String> {
        words(text)
    }

    #[test]
    fn identical_text_scores_one() {
        let a = w("The quick brown fox, the lazy dog.");
        assert_eq!(f1(overlap(&a, &a), a.len(), a.len()), 1.0);
        assert_eq!(order_score(&a, &a), Some((1.0, a.len())));
    }

    #[test]
    fn disjoint_text_scores_zero_and_has_no_order() {
        let a = w("alpha beta");
        let b = w("gamma delta");
        assert_eq!(f1(overlap(&a, &b), a.len(), b.len()), 0.0);
        assert_eq!(order_score(&a, &b), None);
    }

    #[test]
    fn f1_is_blind_to_a_column_read_into_its_neighbour_but_order_is_not() {
        // Two columns. The oracle reads each column down; the extractor reads straight across.
        let oracle = w("l1 l2 l3 l4 r1 r2 r3 r4");
        let across = w("l1 r1 l2 r2 l3 r3 l4 r4");
        assert_eq!(
            f1(overlap(&across, &oracle), across.len(), oracle.len()),
            1.0
        );
        let (order, shared) = order_score(&across, &oracle).unwrap();
        assert_eq!(shared, 8);
        assert!(order < 0.75, "order {order}");
    }

    #[test]
    fn repeated_words_count_once_per_occurrence() {
        let a = w("a a a b");
        let b = w("a b b");
        assert_eq!(overlap(&a, &b), 2);
    }

    #[test]
    fn empty_pages_agree() {
        assert_eq!(f1(0, 0, 0), 1.0);
        assert_eq!(f1(0, 3, 0), 0.0);
    }
}

// ---------------------------------------------------------------------------------------------
// Corpus pin
// ---------------------------------------------------------------------------------------------

struct Entry {
    name: String,
    path: PathBuf,
    sha256: String,
}

struct Corpus {
    entries: Vec<Entry>,
    source: String,
    commit: String,
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn harness_dir() -> PathBuf {
    crate_dir().join("tests/fixtures/extract-quality")
}

fn load_corpus() -> Corpus {
    let manifest_path = match std::env::var_os("RIGHTKIT_PDF_QUALITY_CORPUS") {
        Some(dir) => PathBuf::from(dir).join("manifest.json"),
        None => harness_dir().join("manifest.json"),
    };
    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        panic!(
            "corpus manifest {} unreadable: {e}",
            manifest_path.display()
        )
    });
    let manifest: Value = serde_json::from_str(&text).expect("manifest is JSON");
    let base = manifest_path.parent().expect("manifest has a directory");
    let root = base.join(manifest["root"].as_str().unwrap_or("."));
    let entries = manifest["files"]
        .as_array()
        .expect("manifest has a files array")
        .iter()
        .map(|file| {
            let name = file["path"].as_str().expect("file path").to_string();
            Entry {
                path: root.join(&name),
                sha256: file["sha256"].as_str().expect("file sha256").to_string(),
                name,
            }
        })
        .collect();
    Corpus {
        entries,
        source: manifest["source"].as_str().unwrap_or("").to_string(),
        commit: manifest["commit"].as_str().unwrap_or("").to_string(),
    }
}

/// Every pinned file must exist and match its digest: a drifted corpus is a failure.
fn verify_pin(corpus: &Corpus) -> HashMap<String, Vec<u8>> {
    let mut problems = Vec::new();
    let mut bytes_by_name = HashMap::new();
    for entry in &corpus.entries {
        match std::fs::read(&entry.path) {
            Err(e) => problems.push(format!("{}: unreadable ({e})", entry.name)),
            Ok(bytes) => {
                let digest = sha256_hex(&bytes);
                if digest != entry.sha256 {
                    problems.push(format!(
                        "{}: sha256 {digest} does not match the pin {}",
                        entry.name, entry.sha256
                    ));
                } else {
                    bytes_by_name.insert(entry.name.clone(), bytes);
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "corpus pin violated:\n  {}",
        problems.join("\n  ")
    );
    bytes_by_name
}

// ---------------------------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------------------------

fn pdftotext() -> PathBuf {
    std::env::var_os("RIGHTKIT_PDFTOTEXT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("pdftotext"))
}

/// The oracle's version line, or an error when it cannot be run.
fn oracle_version() -> Result<String, String> {
    let output = Command::new(pdftotext())
        .arg("-v")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", pdftotext().display()))?;
    // poppler prints its version on stderr.
    let text = String::from_utf8_lossy(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    })
    .into_owned();
    Ok(text.lines().next().unwrap_or("").trim().to_string())
}

/// Text per page as poppler reads it (form feed separated).
fn oracle_pages(path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new(pdftotext())
        .args(["-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .map_err(|e| format!("cannot run pdftotext: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "pdftotext failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut pages: Vec<String> = text.split('\u{c}').map(str::to_string).collect();
    // The output ends with a form feed, which leaves one empty piece after the last page.
    if pages.len() > 1 && pages.last().is_some_and(|p| p.trim().is_empty()) {
        pages.pop();
    }
    Ok(pages)
}

// ---------------------------------------------------------------------------------------------
// Measuring
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct FileScore {
    name: String,
    /// Pages compared one to one (false: the page counts differ and the whole text was compared).
    per_page: bool,
    pages: usize,
    f1: f64,
    /// `None` when no page shared a word with the oracle.
    order: Option<f64>,
    note: Option<String>,
}

fn score_file(name: &str, bytes: &[u8], oracle: Vec<String>) -> FileScore {
    let extracted = match rightkit_pdf::extract::extract_text_by_pages(bytes) {
        Ok(pages) => pages,
        Err(error) => {
            return FileScore {
                name: name.to_string(),
                per_page: false,
                pages: oracle.len(),
                f1: 0.0,
                order: None,
                note: Some(format!("extract failed: {error}")),
            }
        }
    };
    let per_page = extracted.len() == oracle.len();
    let (left, right): (Vec<String>, Vec<String>) = if per_page {
        (extracted, oracle)
    } else {
        (vec![extracted.join(" ")], vec![oracle.join(" ")])
    };
    let (mut shared, mut extracted_total, mut oracle_total) = (0usize, 0usize, 0usize);
    let (mut order_sum, mut order_weight) = (0.0f64, 0usize);
    for (a, b) in left.iter().zip(&right) {
        let a = scoring::words(a);
        let b = scoring::words(b);
        shared += scoring::overlap(&a, &b);
        extracted_total += a.len();
        oracle_total += b.len();
        if let Some((score, weight)) = scoring::order_score(&a, &b) {
            order_sum += score * weight as f64;
            order_weight += weight;
        }
    }
    FileScore {
        name: name.to_string(),
        per_page,
        pages: right.len(),
        f1: scoring::f1(shared, extracted_total, oracle_total),
        order: (order_weight > 0).then(|| order_sum / order_weight as f64),
        note: (!per_page).then(|| "page counts differ, whole text compared".to_string()),
    }
}

fn scores_json(scores: &[FileScore], oracle: &str, tolerance: f64) -> Value {
    let mut files = BTreeMap::new();
    for score in scores {
        files.insert(
            score.name.clone(),
            json!({ "f1": score.f1, "order": score.order }),
        );
    }
    json!({ "recorded": true, "tolerance": tolerance, "oracle": oracle, "files": files })
}

#[test]
#[ignore = "needs the external oracle: poppler pdftotext (RIGHTKIT_PDFTOTEXT or PATH); see tests/fixtures/extract-quality/README.md"]
fn extract_quality_vs_pdftotext() {
    let corpus = load_corpus();
    let bytes_by_name = verify_pin(&corpus);
    let version = match oracle_version() {
        Ok(version) => version,
        Err(error) => {
            if std::env::var("RIGHTKIT_PDF_QUALITY_ALLOW_SKIP").as_deref() == Ok("1") {
                eprintln!(
                    "SKIPPED extract quality: oracle missing ({error}); nothing was measured"
                );
                return;
            }
            panic!("oracle missing: {error}. This is a failure, not a pass. Install poppler or set RIGHTKIT_PDFTOTEXT.");
        }
    };
    println!(
        "oracle: {version}\ncorpus: {} @ {} ({} files)",
        corpus.source,
        corpus.commit,
        corpus.entries.len()
    );

    let mut scores = Vec::new();
    for entry in &corpus.entries {
        let oracle = oracle_pages(&entry.path)
            .unwrap_or_else(|e| panic!("oracle failed on {}: {e}", entry.name));
        scores.push(score_file(&entry.name, &bytes_by_name[&entry.name], oracle));
    }

    println!(
        "{:<40} {:>5} {:>8} {:>8} {:>6}  note",
        "file", "pages", "F1", "order", "mode"
    );
    for score in &scores {
        println!(
            "{:<40} {:>5} {:>8.4} {:>8} {:>6}  {}",
            score.name,
            score.pages,
            score.f1,
            score.order.map_or("n/a".to_string(), |o| format!("{o:.4}")),
            if score.per_page { "page" } else { "whole" },
            score.note.as_deref().unwrap_or("")
        );
    }

    let tolerance = 0.02;
    if let Some(path) = std::env::var_os("RIGHTKIT_PDF_QUALITY_RECORD") {
        let text =
            serde_json::to_string_pretty(&scores_json(&scores, &version, tolerance)).unwrap();
        std::fs::write(&path, text + "\n").expect("write the recorded baseline");
        println!(
            "baseline written to {}; review it and copy it over baseline.json",
            PathBuf::from(path).display()
        );
    }

    // Against the recorded baseline.
    let baseline: Value = serde_json::from_str(
        &std::fs::read_to_string(harness_dir().join("baseline.json")).expect("baseline.json"),
    )
    .expect("baseline.json is JSON");
    let mut regressions = Vec::new();
    if baseline["recorded"].as_bool() == Some(true) {
        let tolerance = baseline["tolerance"].as_f64().unwrap_or(tolerance);
        for score in &scores {
            let Some(base) = baseline["files"].get(&score.name) else {
                println!("NOTICE: {} has no baseline entry", score.name);
                continue;
            };
            if let Some(base_f1) = base["f1"].as_f64() {
                if score.f1 < base_f1 - tolerance {
                    regressions.push(format!(
                        "{}: F1 {:.4} under baseline {base_f1:.4}",
                        score.name, score.f1
                    ));
                }
            }
            if let (Some(base_order), Some(order)) = (base["order"].as_f64(), score.order) {
                if order < base_order - tolerance {
                    regressions.push(format!(
                        "{}: order {order:.4} under baseline {base_order:.4}",
                        score.name
                    ));
                }
            }
        }
    } else {
        println!("NOTICE: BASELINE NOT RECORDED. Run with RIGHTKIT_PDF_QUALITY_RECORD=<file> and commit the result.");
    }

    // The decision the issue asks for.
    let orders: Vec<f64> = scores.iter().filter_map(|s| s.order).collect();
    let mean = if orders.is_empty() {
        1.0
    } else {
        orders.iter().sum::<f64>() / orders.len() as f64
    };
    let worst = orders.iter().cloned().fold(1.0f64, f64::min);
    if mean < 0.95 || worst < 0.90 {
        println!(
            "VERDICT: COLUMN-AWARE ORDERING RECOMMENDED (mean order {mean:.4}, worst {worst:.4})"
        );
    } else {
        println!("VERDICT: column-aware ordering not indicated on this corpus (mean order {mean:.4}, worst {worst:.4})");
    }

    assert!(
        scores.iter().all(|s| s
            .note
            .as_deref()
            .is_none_or(|n| !n.starts_with("extract failed"))),
        "extraction failed on at least one file (see the table)"
    );
    assert!(
        regressions.is_empty(),
        "extraction quality regressed:\n  {}",
        regressions.join("\n  ")
    );
}

/// Writes `manifest.json` for the PDFs under `RIGHTKIT_PDF_QUALITY_PIN_DIR`, offline.
#[test]
#[ignore = "writes a manifest into the directory named by RIGHTKIT_PDF_QUALITY_PIN_DIR"]
fn pin_corpus_manifest() {
    let dir = PathBuf::from(
        std::env::var_os("RIGHTKIT_PDF_QUALITY_PIN_DIR").expect("set RIGHTKIT_PDF_QUALITY_PIN_DIR"),
    );
    let source = std::env::var("RIGHTKIT_PDF_QUALITY_SOURCE").expect(
        "set RIGHTKIT_PDF_QUALITY_SOURCE, for example \"mozilla/pdf.js@<commit> test/pdfs\"",
    );
    let mut found: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("read the corpus directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    assert!(!found.is_empty(), "no PDF files under {}", dir.display());
    let files: Vec<Value> = found
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&dir)
                .expect("under the corpus directory")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(path).expect("read a corpus file");
            json!({ "path": relative, "sha256": sha256_hex(&bytes) })
        })
        .collect();
    let manifest = json!({ "source": source, "commit": source, "root": ".", "files": files });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .expect("write manifest.json");
    println!(
        "pinned {} files in {}",
        files.len(),
        dir.join("manifest.json").display()
    );
}
