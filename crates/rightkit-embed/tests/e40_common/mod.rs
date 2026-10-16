//! Shared by `e40_resident_bench.rs` and `e40_token_dump.rs`, so both run the same six texts.
//! Not a test target (it sits in a subdirectory); include it with `mod e40_common;`.

#![allow(dead_code)] // each including test uses a different part

use rightkit_embed::LoadTimings;

/// An input path from the environment, else from the gitignored `tests/inputs.local.json`.
pub fn input(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(Into::into).or_else(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/inputs.local.json");
        let local: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        local[key].as_str().map(Into::into)
    })
}

/// Roughly 10, 25, 50, 100, 200 and 450 tokens: one text per graph bucket (32..512).
pub fn cases() -> Vec<(&'static str, String)> {
    let sentence = "The nightly billing job reconciles every invoice against the ledger. ";
    vec![
        ("q-short", "Q3 forecast discussion and budget meeting".to_owned()),
        ("~32", sentence.repeat(2)),
        ("~64", sentence.repeat(4)),
        ("~128", sentence.repeat(8)),
        ("~256", sentence.repeat(17)),
        ("~512", sentence.repeat(36)),
    ]
}

fn ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Prints `Embedder::load_timings` as one `LOAD ...` line per phase.
pub fn print_load_timings(timings: Option<&LoadTimings>) {
    let Some(t) = timings else {
        eprintln!("LOAD timings: none for this bundle");
        return;
    };
    let opt = |value: Option<std::time::Duration>| {
        value.map_or_else(|| "-".to_owned(), |d| format!("{:.0} ms", ms(d)))
    };
    eprintln!("LOAD total            {:.0} ms", ms(t.total));
    eprintln!("LOAD tokenizer load   {:.0} ms", ms(t.tokenizer_load));
    eprintln!("LOAD token table read {:.0} ms", ms(t.token_table_read));
    eprintln!("LOAD probe            {}", opt(t.probe));
    eprintln!("LOAD parity total     {}", opt(t.parity_check_total));
    eprintln!("LOAD   cpu reference  {}", opt(t.parity_cpu_reference));
    eprintln!("LOAD   accelerator    {}", opt(t.parity_accelerator));
    eprintln!("LOAD parity cache     {:?}", t.parity_cache);
    eprintln!("LOAD prewarm          {}", opt(t.prewarm));
    let mut graph_total = 0.0;
    for create in &t.session_creates {
        graph_total += ms(create.duration);
        eprintln!(
            "LOAD session {:>8} graph {:>3}: {:.0} ms",
            create.provider,
            create.graph_tokens,
            ms(create.duration)
        );
    }
    eprintln!(
        "LOAD session creates  {} graphs, {:.0} ms in all",
        t.session_creates.len(),
        graph_total
    );
}
