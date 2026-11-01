//! Resolve a spoken app name ("open figma") to a launchable target.
//!
//! macOS scans installed apps **live, at command time** (the standard app
//! folders) so every installed app works with no configuration and newly-
//! installed apps are found immediately — no cache to go stale, no per-app
//! setup. (A directory read is sub-millisecond and never blocks; `mdfind` was
//! avoided because it can stall for seconds while Spotlight is mid-indexing,
//! which would freeze the pill.) The match is conservative: a stray
//! "open the door" finds no app and falls through to normal dictation. Windows
//! uses curated aliases plus exact full names from a live Start-menu scan.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(target_os = "windows")]
static WINDOWS_APPS: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// macOS Shortcuts are probed by the streaming command lane, so keep the
/// `shortcuts list` subprocess off that ASR worker. `None` means the async
/// prewarm has not produced a usable snapshot yet; an empty `Vec` is a ready
/// snapshot with no installed shortcuts.
#[cfg(target_os = "macos")]
static MAC_SHORTCUTS: OnceLock<std::sync::RwLock<Option<Vec<String>>>> = OnceLock::new();

#[cfg(target_os = "macos")]
static MAC_SHORTCUT_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
static MAC_SHORTCUT_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
static MAC_SHORTCUT_LAST_ATTEMPT: OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
    OnceLock::new();

#[cfg(target_os = "macos")]
static MAC_SHORTCUT_LAST_SUCCESS: OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
    OnceLock::new();

#[cfg(target_os = "macos")]
const SHORTCUT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Background streaming refresh: a slow `shortcuts list` is dropped quickly so it
/// never holds the indexer thread.
#[cfg(target_os = "macos")]
const SHORTCUT_SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Final lookup: a cold `shortcuts list` can take well over 500 ms, and a timeout
/// here must not turn an installed shortcut into "not found".
#[cfg(target_os = "macos")]
const SHORTCUT_FINAL_SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(target_os = "macos")]
const MAX_SHORTCUT_OUTPUT_BYTES: usize = 256 * 1024;

use rightkit_voice_commands::command_recognition::edit_distance;

/// Minimum match score to actually launch — below this we return `None` so the
/// words get typed as dictation instead of firing a wrong app.
const MATCH_THRESHOLD: i32 = 50;

/// Collapse to lowercase alphanumerics only — drops spaces AND punctuation so a
/// single-word app name spoken as several words ("view right" / "view, right?"
/// -> "viewright") compares equal. This is the key to matching Right Suite app
/// names, which are one CamelCase word the ASR reliably splits.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Resolve `query` (the raw spoken app name) to a launch target string. On macOS
/// that's an installed app's display name (for `open -a`); on Windows it's the
/// curated alias or exact Start-menu app target.
#[cfg(target_os = "macos")]
pub fn resolve(query: &str) -> Option<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 {
        return None;
    }
    best_match(&q, &scan_macos_apps())
}

#[cfg(target_os = "windows")]
pub fn resolve(query: &str) -> Option<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 {
        return None;
    }
    // 1. Curated common apps + system tools (chrome, settings, cmd…) — exact + cheap.
    if let Some(alias) = rightkit_voice_commands::command_recognition::app_alias(&q) {
        return Some(alias.to_string());
    }
    // 2. Live Start-Menu scan, fuzzy. This is the FINAL command-time resolve
    //    (the "open" verb is already confirmed, ASR is no longer revising), so
    //    fuzzy is safe here — and necessary: a one-word app name the ASR splits
    //    ("ViewRight" -> "view right"/"view, right?") only matches with the
    //    space/punctuation-insensitive scorer, not an exact string compare. The
    //    conservative exact-only path stays on `resolve_streaming` below, where
    //    the last word can still flip. Cache is prewarmed beside ASR startup.
    best_windows_match(&q, windows_apps())
}

/// Fuzzy best match over the live Start-menu index, scored by display name and
/// returning the launch target. Same shape/threshold as macOS `best_match`.
#[cfg(target_os = "windows")]
fn best_windows_match(q: &str, apps: &[(String, String)]) -> Option<String> {
    best_match_generic(q, apps, |app| app.0.clone()).map(|app| app.1.clone())
}

#[cfg(target_os = "windows")]
pub fn resolve_streaming(query: &str) -> Option<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 {
        return None;
    }
    if let Some(alias) = rightkit_voice_commands::command_recognition::app_alias(&q) {
        return Some(alias.to_string());
    }
    WINDOWS_APPS
        .get()
        .and_then(|apps| certain_windows_match(&q, apps))
}

/// Streaming fires only on a CERTAIN match — exact, or space/punctuation-
/// insensitive equal (`score_match` == 100, e.g. "view right" -> "viewright").
/// The looser fuzzy scores (whole-word, prefix, edit-distance) are deliberately
/// withheld until the final `resolve` so a still-revising last word can't fire
/// the wrong app mid-hold. This is what lets "open ViewRight" fire while the PTT
/// key is still held, like every other app, instead of only on release.
#[cfg(target_os = "windows")]
fn certain_windows_match(q: &str, apps: &[(String, String)]) -> Option<String> {
    apps.iter()
        .find(|(name, _)| score_match(q, name) >= 100)
        .map(|(_, target)| target.clone())
}

#[cfg(target_os = "macos")]
pub fn resolve_streaming(query: &str) -> Option<String> {
    resolve(query)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn resolve_streaming(_query: &str) -> Option<String> {
    None
}

pub fn prewarm() {
    #[cfg(target_os = "macos")]
    {
        request_shortcut_refresh(true);
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::thread::Builder::new()
            .name("rightkit-app-index".into())
            .spawn(|| {
                let started = std::time::Instant::now();
                let apps = windows_apps();
                tracing::info!(
                    app_count = apps.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "Windows app index ready"
                );
            });
    }
}

#[cfg(target_os = "windows")]
fn windows_apps() -> &'static Vec<(String, String)> {
    WINDOWS_APPS.get_or_init(scan_windows_apps)
}

/// Every installed Win32 & Store app as `(lowercased display name, launch target)`.
/// Enumerates the shell's native AppsFolder, avoiding a PowerShell process while
/// retaining the same Store-app coverage as the Start menu.
#[cfg(target_os = "windows")]
fn scan_windows_apps() -> Vec<(String, String)> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem,
        KF_FLAG_DEFAULT, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    };

    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            // SAFETY: paired with successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }

    // SAFETY: this dedicated indexing thread has not initialized COM yet.
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        return Vec::new();
    }
    let _com = ComGuard;
    // SAFETY: known-folder ID is static; returned COM pointers are RAII-owned.
    let Ok(folder) = (unsafe {
        SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
    }) else {
        return Vec::new();
    };
    let Ok(items) =
        (unsafe { folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems) })
    else {
        return Vec::new();
    };

    let display_name = |item: &IShellItem, kind| {
        // SAFETY: shell allocates the returned PWSTR with CoTaskMemAlloc.
        let value = unsafe { item.GetDisplayName(kind) }.ok()?;
        let text = unsafe { value.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        text.map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    };

    let mut apps = Vec::new();
    loop {
        let mut slot = [None];
        let mut fetched = 0_u32;
        // SAFETY: slot has capacity for one item & fetched points to writable u32.
        if unsafe { items.Next(&mut slot, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        let Some(item) = slot[0].take() else {
            continue;
        };
        let Some(name) = display_name(&item, SIGDN_NORMALDISPLAY) else {
            continue;
        };
        let Some(target) = display_name(&item, SIGDN_DESKTOPABSOLUTEPARSING) else {
            continue;
        };
        apps.push((name.to_ascii_lowercase(), target));
    }
    apps
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn resolve(_query: &str) -> Option<String> {
    None
}

/// Resolve a spoken shortcut name to an installed macOS Shortcut's exact name.
/// Final classification remains authoritative: it scans the live catalog, then
/// refreshes the streaming snapshot so newly-created shortcuts become usable by
/// the next recording. If the scan fails or times out, the last good snapshot
/// answers instead of reporting the shortcut missing.
#[cfg(target_os = "macos")]
pub fn resolve_shortcut(query: &str) -> Option<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 {
        return None;
    }
    let generation = MAC_SHORTCUT_CACHE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let scan = scan_shortcuts(SHORTCUT_FINAL_SCAN_TIMEOUT);
    let cached = shortcut_cache().read().ok().and_then(|cache| cache.clone());
    let resolved = resolve_shortcut_with(&q, scan.clone(), cached.as_deref());
    if let Some(shortcuts) = scan {
        store_shortcut_cache(generation, shortcuts);
    }
    resolved
}

/// Match `q` (trimmed, lowercased) against the live scan. `scan` is `None` only
/// when the probe failed: an authoritative empty list wins over the cache, but a
/// failed probe falls back to the last completed snapshot.
#[cfg(any(test, target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn resolve_shortcut_with(
    q: &str,
    scan: Option<Vec<String>>,
    cached: Option<&[String]>,
) -> Option<String> {
    let shortcuts: &[String] = match (&scan, cached) {
        (Some(scanned), _) => scanned,
        (None, Some(cached)) => cached,
        (None, None) => return None,
    };
    best_match_generic(q, shortcuts, |name| name.to_ascii_lowercase()).cloned()
}

/// Resolve a streaming shortcut query from the last completed snapshot. The
/// match is exact (including punctuation-insensitive equality) and must be
/// unique; fuzzy or ambiguous matches wait for final authoritative resolution.
#[cfg(target_os = "macos")]
pub fn resolve_shortcut_streaming(query: &str) -> Option<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 {
        return None;
    }
    let stale = shortcut_cache_is_stale();
    let resolved = shortcut_cache().read().ok().and_then(|cache| {
        (!stale)
            .then(|| {
                cache
                    .as_ref()
                    .and_then(|shortcuts| certain_shortcut_match(&q, shortcuts))
            })
            .flatten()
    });
    if stale || resolved.is_none() {
        request_shortcut_refresh(false);
    }
    resolved
}

#[cfg(target_os = "macos")]
pub fn shortcut_streaming_is_ambiguous(query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.chars().count() < 2 || shortcut_cache_is_stale() {
        return false;
    }
    let Ok(cache) = shortcut_cache().read() else {
        return false;
    };
    let Some(shortcuts) = cache.as_ref() else {
        return false;
    };
    let Some(candidate) = certain_shortcut_match(&q, shortcuts) else {
        return false;
    };
    let squashed_query = squash(&q);
    shortcuts.iter().any(|shortcut| {
        let lower = shortcut.to_ascii_lowercase();
        lower != candidate.to_ascii_lowercase()
            && !squashed_query.is_empty()
            && squash(&lower).starts_with(&squashed_query)
            && squash(&lower).len() > squashed_query.len()
    })
}

#[cfg(not(target_os = "macos"))]
pub fn resolve_shortcut_streaming(_query: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn shortcut_streaming_is_ambiguous(_query: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn shortcut_cache() -> &'static std::sync::RwLock<Option<Vec<String>>> {
    MAC_SHORTCUTS.get_or_init(|| std::sync::RwLock::new(None))
}

#[cfg(target_os = "macos")]
fn store_shortcut_cache(generation: u64, shortcuts: Vec<String>) {
    let Ok(mut cache) = shortcut_cache().write() else {
        return;
    };
    // Recheck after acquiring write lock: an authoritative final scan may
    // have advanced generation while this background scan waited here.
    if MAC_SHORTCUT_CACHE_GENERATION.load(Ordering::Acquire) != generation {
        return;
    }
    *cache = Some(shortcuts);
    drop(cache);
    if let Ok(mut last_success) = MAC_SHORTCUT_LAST_SUCCESS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *last_success = Some(std::time::Instant::now());
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_shortcut_cache_for_tests(shortcuts: Option<Vec<String>>) {
    MAC_SHORTCUT_CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
    let has_snapshot = shortcuts.is_some();
    if let Ok(mut cache) = shortcut_cache().write() {
        *cache = shortcuts;
    }
    if let Ok(mut last_success) = MAC_SHORTCUT_LAST_SUCCESS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *last_success = has_snapshot.then(std::time::Instant::now);
    }
    if let Ok(mut last_attempt) = MAC_SHORTCUT_LAST_ATTEMPT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *last_attempt = None;
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_shortcut_refresh_times_for_tests(
    last_success: Option<std::time::Instant>,
    last_attempt: Option<std::time::Instant>,
) {
    if let Ok(mut value) = MAC_SHORTCUT_LAST_SUCCESS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *value = last_success;
    }
    if let Ok(mut value) = MAC_SHORTCUT_LAST_ATTEMPT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *value = last_attempt;
    }
}

#[cfg(target_os = "macos")]
fn shortcut_cache_is_stale() -> bool {
    let ready = shortcut_cache()
        .read()
        .ok()
        .is_some_and(|cache| cache.is_some());
    if !ready {
        return false;
    }
    MAC_SHORTCUT_LAST_SUCCESS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|last| *last)
        .is_none_or(|last| last.elapsed() >= SHORTCUT_REFRESH_INTERVAL)
}

#[cfg(target_os = "macos")]
fn request_shortcut_refresh(force: bool) {
    if MAC_SHORTCUT_REFRESH_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }

    let now = std::time::Instant::now();
    if !force {
        let last_attempt = MAC_SHORTCUT_LAST_ATTEMPT.get_or_init(|| std::sync::Mutex::new(None));
        let within_interval = last_attempt
            .lock()
            .ok()
            .and_then(|last| *last)
            .is_some_and(|last| now.duration_since(last) < SHORTCUT_REFRESH_INTERVAL);
        if within_interval {
            MAC_SHORTCUT_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
            return;
        }
    }
    if let Ok(mut last_attempt) = MAC_SHORTCUT_LAST_ATTEMPT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *last_attempt = Some(now);
    }

    let generation = MAC_SHORTCUT_CACHE_GENERATION.load(Ordering::Acquire);
    let spawn = std::thread::Builder::new()
        .name("rightkit-shortcut-index".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            if let Some(shortcuts) = scan_shortcuts(SHORTCUT_SCAN_TIMEOUT) {
                let count = shortcuts.len();
                store_shortcut_cache(generation, shortcuts);
                tracing::info!(
                    shortcut_count = count,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "macOS shortcut index ready"
                );
            } else {
                tracing::warn!("macOS shortcut index unavailable");
            }
            MAC_SHORTCUT_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        });
    if spawn.is_err() {
        MAC_SHORTCUT_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Streaming certainty contract: a canonical name or a punctuation/spacing
/// equivalent may fire, but two catalog entries mapping to the same spoken
/// form are withheld as ambiguous.
#[cfg(target_os = "macos")]
fn certain_shortcut_match(query: &str, shortcuts: &[String]) -> Option<String> {
    let squashed_query = squash(query);
    let mut found: Option<&String> = None;
    for shortcut in shortcuts {
        let lower = shortcut.to_ascii_lowercase();
        if lower != query && (squashed_query.is_empty() || squash(&lower) != squashed_query) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(shortcut);
    }
    found.cloned()
}

/// Installed Shortcuts by name, one per line from `shortcuts list`. `None`
/// distinguishes a failed probe from a successfully empty catalog, allowing a
/// prior ready snapshot to remain intact after a transient CLI failure.
#[cfg(target_os = "macos")]
fn scan_shortcuts(timeout: std::time::Duration) -> Option<Vec<String>> {
    use rightkit_child_process::{run, RunOptions};

    let output = run(
        "shortcuts",
        ["list"],
        &RunOptions::new(timeout).stdout_cap(MAX_SHORTCUT_OUTPUT_BYTES),
    )
    .ok()?;
    if output.timed_out || !output.status.success() || output.stdout.truncated {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout.bytes)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

#[cfg(not(target_os = "macos"))]
pub fn resolve_shortcut(_query: &str) -> Option<String> {
    None
}

/// List installed app display names by reading the standard app folders. Fast
/// (a few directory reads) and non-blocking, run fresh on each launch command.
#[cfg(target_os = "macos")]
fn scan_macos_apps() -> Vec<String> {
    let mut names = Vec::new();
    for dir in macos_app_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path().to_string_lossy().to_string();
                if is_user_facing_app_path(&path) {
                    if let Some(name) = app_name_from_path(&path) {
                        names.push(name);
                    }
                }
            }
        }
    }
    names
}

/// Keep only real user-facing app bundles: a `.app` directly in one of the scan
/// folders, never a helper nested inside another bundle (`…/Contents/…`,
/// `…/Frameworks/…`) or a system service under `…/Library/…`.
#[cfg(target_os = "macos")]
fn is_user_facing_app_path(path: &str) -> bool {
    let p = path.trim();
    p.ends_with(".app")
        && !p.contains("/Library/")
        && !p.contains("/Contents/")
        && !p.contains("/Frameworks/")
}

#[cfg(target_os = "macos")]
fn macos_app_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![
        std::path::PathBuf::from("/Applications"),
        std::path::PathBuf::from("/Applications/Utilities"),
        std::path::PathBuf::from("/System/Applications"),
        std::path::PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = crate::user_dirs::account_home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// "/Applications/Google Chrome.app" -> "Google Chrome". `file_stem` drops the
/// trailing ".app".
#[cfg(target_os = "macos")]
fn app_name_from_path(path: &str) -> Option<String> {
    let p = path.trim();
    if !p.ends_with(".app") {
        return None;
    }
    std::path::Path::new(p)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[cfg(target_os = "macos")]
fn best_match(q: &str, apps: &[String]) -> Option<String> {
    best_match_generic(q, apps, |app| app.to_ascii_lowercase()).cloned()
}

/// Fuzzy best match shared by the macOS and Windows resolvers: highest
/// `score_match` against `name_of(item)` wins; ties go to the shorter
/// (more specific) scored name. `name_of` extracts what gets scored/compared
/// from a candidate — the lowercased app name on macOS, the display name on
/// Windows — so each platform's existing lookup key stays exactly as it was.
fn best_match_generic<'a, T>(
    q: &str,
    items: &'a [T],
    name_of: impl Fn(&T) -> String,
) -> Option<&'a T> {
    let mut best: Option<(i32, &T)> = None;
    for item in items {
        let score = score_match(q, &name_of(item));
        if score <= 0 {
            continue;
        }
        match best {
            Some((bs, bi))
                if score > bs || (score == bs && name_of(item).len() < name_of(bi).len()) =>
            {
                best = Some((score, item))
            }
            None => best = Some((score, item)),
            _ => {}
        }
    }
    best.filter(|(s, _)| *s >= MATCH_THRESHOLD)
        .map(|(_, item)| item)
}

/// Conservative similarity score between the spoken query and a lowercased app
/// name. 0 = no match. Tuned so common spoken names hit and stray words don't.
fn score_match(q: &str, name: &str) -> i32 {
    let qn = q.chars().count();
    // Exact full-name match — always safe ("figma" -> "Figma").
    if name == q {
        return 100;
    }
    // Space/punctuation-insensitive equality: a one-word app name the ASR split
    // into several words ("view right"/"view, right?" -> "viewright") is a
    // certain match. Gated to >= 3 squashed chars so tiny words can't collide.
    let sq = squash(q);
    if sq.chars().count() >= 3 && sq == squash(name) {
        return 100;
    }
    // Whole-word exact match is the workhorse ("chrome" -> "Google Chrome",
    // "code" -> "Visual Studio Code"). Require >= 4 chars so short common words
    // ("mic", "it", "pro", "vis") can never fire an app launch.
    if qn >= 4 && name.split_whitespace().any(|w| w == q) {
        return 90;
    }
    // Longer fuzzy aids only — gated to >= 5 chars so short English syllables
    // ("mic" -> Microsoft, "loom" -> Zoom) don't collide.
    if qn >= 5 {
        if name.split_whitespace().any(|w| w.starts_with(q)) {
            return 75;
        }
        if name.starts_with(q) {
            return 72;
        }
        if edit_distance(q, name) <= 1 {
            return 70; // single-word mishears: "spotofy" -> "Spotify"
        }
        if name.split_whitespace().any(|w| edit_distance(q, w) <= 1) {
            return 65;
        }
    }
    // Substring only for longer, specific queries ("screen" -> "ScreenFlow").
    if qn >= 6 && name.contains(q) {
        return 60;
    }
    0
}

#[cfg(test)]
mod shortcut_fallback_tests {
    use super::resolve_shortcut_with;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn failed_scan_falls_back_to_the_cached_snapshot() {
        let cached = names(&["Morning Routine"]);
        assert_eq!(
            resolve_shortcut_with("morning routine", None, Some(&cached)),
            Some("Morning Routine".to_string())
        );
    }

    #[test]
    fn failed_scan_without_a_cache_resolves_nothing() {
        assert_eq!(resolve_shortcut_with("morning routine", None, None), None);
    }

    #[test]
    fn authoritative_empty_scan_beats_the_cache() {
        let cached = names(&["Morning Routine"]);
        assert_eq!(
            resolve_shortcut_with("morning routine", Some(Vec::new()), Some(&cached)),
            None
        );
    }

    #[test]
    fn successful_scan_resolves() {
        assert_eq!(
            resolve_shortcut_with("morning routine", Some(names(&["Morning Routine"])), None),
            Some("Morning Routine".to_string())
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "app_launch_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "windows"))]
#[path = "app_launch_windows_tests.rs"]
mod windows_tests;
