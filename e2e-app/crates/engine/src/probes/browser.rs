//! rightkit-browser: Chrome resolution as the runtime does it (Chrome for Testing candidates, then an installed Chrome), and a real
//! headless launch of what it found when one exists. No Chrome on the host is reported with the lookup that was made.
use rightkit_browser::{chrome_for_testing_candidates, find_chrome};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let candidates = chrome_for_testing_candidates();
    let chrome = find_chrome().ok_or_else(|| format!("needs Chrome: no Chrome for Testing in {candidates:?} and no installed Chrome found by find_chrome()"))?;
    let profile = ctx.dir("browser")?;
    let out = std::process::Command::new(&chrome)
        .args(["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--dump-dom", "data:text/html,<p id=x>rke2e</p>"])
        .output()
        .map_err(|e| format!("launch {}: {e}", chrome.display()))?;
    let dom = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !dom.contains("rke2e") {
        return Err(format!("headless Chrome at {} did not render the page (status {:?})", chrome.display(), out.status.code()));
    }
    Ok(json!({ "chrome": chrome, "rendered": true }))
}
