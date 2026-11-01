// rightkit-captions 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves caption building a host relies on: cues built from word timings for a platform preset
// keep within the preset's line, character and duration limits, a pause starts a new cue, cue ids
// are stable for the same input, and SRT export numbers every cue.
// Does not prove: burned-in rendering, font measurement against real fonts, safe-zone placement.
use rightkit_captions::*;
use rightkit_timeline::Preset;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let words: Vec<TimedWord> = [
        ("Welcome", 500, 900),
        ("back", 950, 1200),
        ("to", 1250, 1350),
        ("the", 1400, 1500),
        ("channel.", 1550, 2100),
        ("Today", 2300, 2700),
        ("we", 2750, 2850),
        ("build", 2900, 3300),
        ("an", 3350, 3450),
        ("editor.", 3500, 4100),
        ("No", 6000, 6200),
        ("cloud.", 6250, 6800),
    ]
    .iter()
    .map(|&(t, s, e)| TimedWord::new(t, s, e))
    .collect();
    let cfg = config_for_preset(Preset::YouTube);
    let cues = build_cues(&words, &cfg);
    check(!cues.is_empty(), "cues are built");
    check(
        cues.iter().all(|c| {
            c.lines.len() <= cfg.max_lines
                && c.end_ms - c.start_ms <= cfg.max_cue_ms
                && c.lines
                    .iter()
                    .all(|l| l.chars().count() <= cfg.max_chars_per_line)
        }),
        "every cue keeps within the preset's limits",
    );
    check(
        cues.iter().any(|c| c.start_ms == 6000),
        "a pause starts a new cue",
    );
    check(
        cue_ids(&words, &cues, "en", "source")
            == cue_ids(&words, &build_cues(&words, &cfg), "en", "source"),
        "cue ids are stable for the same input",
    );
    let srt = to_srt(&cues, Newline::Lf);
    check(
        srt.starts_with("1\n") && srt.contains(&format!("\n{}\n", cues.len())),
        "SRT numbers every cue",
    );
    println!("rightkit-captions consumer: all checks passed");
}
