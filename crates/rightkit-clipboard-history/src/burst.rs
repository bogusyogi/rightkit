//! The copy burst .
//!
//! Two copies less than 60 s apart start a burst: one dot per item on the Send cell, in copy
//! order (a single copy shows nothing). While it is up a plain Ctrl+V (or Alt+V where Alt acts as
//! Ctrl) in any app pastes the oldest item not yet pasted, drops its dot and leaves the next one
//! on the clipboard; the last paste ends the burst. Ctrl-click or Alt-click on dots selects several,
//! and the next paste puts all the selected together. The burst also ends after about five idle
//! minutes or when it is dismissed; repeating an item needs a re-copy. Behind
//! `clipboardCopyBurst` (default on). Held in memory for this run; nothing here logs content.
//!
//! The key hook must decide at once, so the clipboard is staged ahead: a worker thread keeps it
//! holding what the next Ctrl+V should paste (the first item, or the selection), and
//! [`burst_paste_key`] only checks that it does (waiting a moment when the worker is still
//! writing) before the keystroke goes on to the app. After a paste the worker restages a short
//! while later, so the app has read the clipboard first.

use crate::host;
use crate::model::{self as clip, Kind};
use crate::paste::{WRITE_WAITS, mark_own, write_back};
use crate::win::{self, Open};
use rightkit_notch_ui::card::{CardContent, Row};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A second copy within this long of the previous one is a burst.
const WINDOW: Duration = Duration::from_secs(60);
/// A burst nobody has copied into or pasted from for this long ends.
const IDLE: Duration = Duration::from_secs(5 * 60);
/// How long the key hook waits for the clipboard to be staged before it lets the key through.
const STAGE_WAIT: Duration = Duration::from_millis(200);
/// How long after a paste the next item goes on the clipboard (the app reads it first).
const RESTAGE_DELAY: u64 = 150;
/// The longest a preview line is.
const PREVIEW_CHARS: usize = 160;

struct Entry {
    id: String,
    selected: bool,
}

struct State {
    /// The latest copy and when it was made: the next copy within `WINDOW` of it joins.
    prev: Option<(String, Instant)>,
    /// What is still to paste, in copy order. Empty means no burst.
    items: Vec<Entry>,
    /// The last copy, paste or dismissal, for the idle end.
    touched: Option<Instant>,
    /// The item ids the clipboard holds as staged, when it holds a staged set.
    staged: Option<Vec<String>>,
    /// A staging thread is running.
    worker: bool,
    /// An idle watcher thread is running.
    watcher: bool,
    /// The dot the pointer is on.
    hover: Option<usize>,
}

static STATE: Mutex<State> = Mutex::new(State {
    prev: None,
    items: Vec::new(),
    touched: None,
    staged: None,
    worker: false,
    watcher: false,
    hover: None,
});
/// Signalled whenever the staged set changes.
static STAGED: Condvar = Condvar::new();
/// Whether a burst is up: the key hook's one cheap check.
static LIVE: AtomicBool = AtomicBool::new(false);
/// The V key is down (a repeat is swallowed so a held key does not eat the burst).
static V_DOWN: AtomicBool = AtomicBool::new(false);

fn locked() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn enabled() -> bool {
    host::switch_on("clipboardCopyBurst")
}

/// What the next paste puts on the clipboard: the selected items, else the first one.
fn wanted(state: &State) -> Option<Vec<String>> {
    let selected: Vec<String> = state
        .items
        .iter()
        .filter(|entry| entry.selected)
        .map(|entry| entry.id.clone())
        .collect();
    if !selected.is_empty() {
        return Some(selected);
    }
    state.items.first().map(|entry| vec![entry.id.clone()])
}

/// Ends the burst: no dots, nothing staged, and the next copy starts afresh.
fn end(state: &mut State) {
    state.items.clear();
    state.staged = None;
    state.hover = None;
    state.prev = None;
    LIVE.store(false, Ordering::Release);
    STAGED.notify_all();
}

fn expired(state: &State) -> bool {
    state.touched.is_some_and(|at| at.elapsed() >= IDLE)
}

// ---- what the notch shows ------------------------------------------------------------------

/// A burst is up (two or more items still to paste).
pub fn burst_active() -> bool {
    LIVE.load(Ordering::Acquire)
}

/// The dots, one per item still to paste in copy order; `true` for a selected one. Empty while no
/// burst is up.
pub fn burst_dots() -> Vec<bool> {
    if !burst_active() {
        return Vec::new();
    }
    let mut state = locked();
    if expired(&state) {
        end(&mut state);
        return Vec::new();
    }
    state.items.iter().map(|entry| entry.selected).collect()
}

/// The pointer is on dot `index`, or left them (`None`): the preview card follows.
pub fn burst_hover(index: Option<usize>) {
    let changed = {
        let mut state = locked();
        let index = index.filter(|&i| i < state.items.len());
        let changed = state.hover != index;
        state.hover = index;
        changed
    };
    if changed {
        host::burst_changed();
    }
}

/// A click on dot `index`. `additive` (Ctrl-click, Alt-click) toggles it in the selection; a plain
/// click selects it alone, or clears the selection when it was the only one selected. The clipboard
/// is restaged for what the next paste will put in.
pub fn burst_click(index: usize, additive: bool) {
    {
        let mut state = locked();
        if index >= state.items.len() {
            return;
        }
        if additive {
            let entry = &mut state.items[index];
            entry.selected = !entry.selected;
        } else {
            let only_this = state
                .items
                .iter()
                .enumerate()
                .all(|(i, entry)| entry.selected == (i == index));
            for (i, entry) in state.items.iter_mut().enumerate() {
                entry.selected = i == index && !only_this;
            }
        }
        state.touched = Some(Instant::now());
        spawn_worker(&mut state, 0);
    }
    host::burst_changed();
}

/// Dismisses the burst (the dots go; whatever is on the clipboard stays).
pub fn burst_dismiss() {
    let was = {
        let mut state = locked();
        let was = !state.items.is_empty();
        end(&mut state);
        was
    };
    if was {
        host::burst_changed();
    }
}

/// The card a hovered dot shows: where the item stands in the burst and a snippet of it. `None`
/// when no dot is hovered. Hangs from the Send cell like its other popups, without expanding it.
pub fn burst_preview_panel() -> Option<CardContent> {
    let (position, total, selected, id) = {
        let state = locked();
        let index = state.hover.filter(|&i| i < state.items.len())?;
        (
            index + 1,
            state.items.len(),
            state.items[index].selected,
            state.items[index].id.clone(),
        )
    };
    let (kind, preview) =
        clip::with(|store| store.get(&id).map(|item| (item.kind, item.preview.clone())))?;
    let mut snippet: String = preview
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PREVIEW_CHARS)
        .collect();
    if snippet.is_empty() {
        snippet = match kind {
            Kind::Image => host::t("Image").to_string(),
            Kind::File => host::t("Files").to_string(),
            _ => host::t("Empty").to_string(),
        };
    }
    // An image shows its thumbnail (a file row over the stored PNG); anything else its text.
    let picture = (kind == Kind::Image)
        .then(|| clip::with(|store| store.payload(&id)).and_then(|payload| payload.png))
        .flatten()
        .map(|png| Row::File {
            name: snippet.clone(),
            path: png.to_string_lossy().into_owned(),
        });
    let mut content = CardContent::plain(
        host::tf(
            "Copy {} of {}",
            &[&position.to_string(), &total.to_string()],
        ),
        vec![picture.unwrap_or(Row::Text(snippet))],
    );
    if selected {
        content.accessory = Some(host::t("Selected").to_string());
    }
    Some(content)
}

// ---- copying -------------------------------------------------------------------------------

/// A copy was captured into the history as item `id` (called from the capture thread). A copy
/// less than 60 s after the previous one starts or extends the burst.
pub fn burst_copied(id: &str) {
    if !enabled() {
        burst_dismiss();
        return;
    }
    let now = Instant::now();
    let started = {
        let mut state = locked();
        let within = state
            .prev
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) < WINDOW);
        let mut started = false;
        if !within {
            // Too long since the last copy: what was left of the old burst is dropped (it stays
            // in the history), and this copy is the first of a new one.
            state.items.clear();
            state.staged = None;
            state.hover = None;
        } else if state.items.is_empty() {
            let previous = state.prev.as_ref().map(|(prev, _)| prev.clone());
            if let Some(previous) = previous {
                state.items.push(Entry {
                    id: previous,
                    selected: false,
                });
                started = true;
            }
        }
        if within {
            state.items.push(Entry {
                id: id.to_string(),
                selected: false,
            });
        }
        state.prev = Some((id.to_string(), now));
        state.touched = Some(now);
        let live = !state.items.is_empty();
        LIVE.store(live, Ordering::Release);
        if live {
            // The clipboard now holds the copy just made; the burst pastes the oldest first.
            state.staged = None;
            spawn_worker(&mut state, 30);
            spawn_watcher(&mut state);
        }
        started
    };
    if started {
        // Files made for the last burst's multi-paste are no longer wanted.
        let _ = std::fs::remove_dir_all(burst_dir());
    }
    host::burst_changed();
}

// ---- pasting -------------------------------------------------------------------------------

/// The V key went up.
pub fn burst_v_up() {
    V_DOWN.store(false, Ordering::Release);
}

/// A plain paste chord (Ctrl+V, or Alt+V acting as Ctrl+V) is about to reach the app, on the key
/// hook thread. While a burst is up it makes sure the clipboard holds what this paste should put
/// in (the oldest item, or the selection), then counts the paste: those dots go and the next item
/// is staged shortly after. Returns true to swallow the key: a repeat of a held V, which would
/// eat the burst. Never blocks long: the key goes through unchanged when the clipboard cannot be
/// staged in time.
pub fn burst_paste_key() -> bool {
    if !burst_active() || !enabled() {
        return false;
    }
    if V_DOWN.swap(true, Ordering::AcqRel) {
        return true;
    }
    let mut state = locked();
    if expired(&state) {
        end(&mut state);
        drop(state);
        host::burst_changed();
        return false;
    }
    let Some(want) = wanted(&state) else {
        return false;
    };
    if state.staged.as_ref() != Some(&want) {
        spawn_worker(&mut state, 0);
        let deadline = Instant::now() + STAGE_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (guard, _) = STAGED
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if wanted(&state).as_ref() == state.staged.as_ref() && state.staged.is_some() {
                break;
            }
        }
        if wanted(&state).as_ref() != state.staged.as_ref() || state.staged.is_none() {
            host::log_info("clipboard", &[("result", "burst_not_staged")]);
            return false;
        }
    }
    // Consume what this paste takes: the selection, else the first item.
    if state.items.iter().any(|entry| entry.selected) {
        state.items.retain(|entry| !entry.selected);
    } else if !state.items.is_empty() {
        state.items.remove(0);
    }
    state.staged = None;
    state.hover = None;
    state.touched = Some(Instant::now());
    if state.items.is_empty() {
        end(&mut state);
    } else {
        spawn_worker(&mut state, RESTAGE_DELAY);
    }
    drop(state);
    host::burst_changed();
    false
}

// ---- staging the clipboard -----------------------------------------------------------------

/// Starts the staging thread unless one is running; it waits `delay_ms`, then keeps writing the
/// wanted set to the clipboard until the clipboard holds it.
fn spawn_worker(state: &mut State, delay_ms: u64) {
    if state.worker || state.items.is_empty() {
        return;
    }
    state.worker = true;
    let spawned = std::thread::Builder::new()
        .name("rightkit-clip-burst".to_string())
        .spawn(move || stage_loop(delay_ms));
    if spawned.is_err() {
        state.worker = false;
        host::log_info("clipboard", &[("result", "no_burst_thread")]);
    }
}

fn stage_loop(delay_ms: u64) {
    crate::img::init_com();
    std::thread::sleep(Duration::from_millis(delay_ms));
    let mut failures = 0;
    loop {
        let want = {
            let mut state = locked();
            match wanted(&state) {
                Some(want) if state.staged.as_ref() != Some(&want) => want,
                _ => {
                    state.worker = false;
                    STAGED.notify_all();
                    return;
                }
            }
        };
        let wrote = write_ids(&want);
        let mut state = locked();
        if wrote {
            failures = 0;
            // Only what the clipboard really holds counts as staged.
            state.staged = Some(want);
            STAGED.notify_all();
        } else {
            failures += 1;
            if failures >= 3 {
                state.worker = false;
                STAGED.notify_all();
                return;
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Watches for the idle end: after five quiet minutes the dots go by themselves.
fn spawn_watcher(state: &mut State) {
    if state.watcher {
        return;
    }
    state.watcher = true;
    let spawned = std::thread::Builder::new()
        .name("rightkit-clip-burst-idle".to_string())
        .spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let mut state = locked();
                if state.items.is_empty() {
                    state.watcher = false;
                    return;
                }
                if expired(&state) {
                    end(&mut state);
                    state.watcher = false;
                    drop(state);
                    host::burst_changed();
                    return;
                }
            }
        });
    if spawned.is_err() {
        state.watcher = false;
    }
}

// ---- writing the clipboard -----------------------------------------------------------------

/// Where the PNGs of a multi-image paste are written (cleared at the next burst).
fn burst_dir() -> std::path::PathBuf {
    std::env::temp_dir()
        .join(crate::host::drag_copy_dir())
        .join("burst")
}

/// Puts the items on the clipboard together: one item as it was copied; several as text joined
/// with newlines, and files and images (as temporary PNGs) as one file list.
fn write_ids(ids: &[String]) -> bool {
    match ids {
        [] => false,
        [one] => write_back(one, false),
        _ => write_many(ids),
    }
}

fn write_many(ids: &[String]) -> bool {
    // Text items and the names of copied files, in copy order.
    let mut lines: Vec<String> = Vec::new();
    let mut has_text = false;
    // The one file list: copied files, then each image's temporary PNG.
    let mut drop_list: Vec<String> = Vec::new();
    for id in ids {
        let Some((kind, payload)) =
            clip::with(|store| Some((store.get(id)?.kind, store.payload(id)?)))
        else {
            continue;
        };
        match kind {
            Kind::Image => {
                let Some(png) = payload.png.as_deref().and_then(crate::seal::read) else {
                    continue;
                };
                let dir = burst_dir();
                let path = dir.join(format!("{id}.png"));
                if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, &png).is_ok() {
                    drop_list.push(path.to_string_lossy().into_owned());
                }
            }
            Kind::File => {
                lines.extend(payload.paths.iter().cloned());
                drop_list.extend(payload.paths);
            }
            _ => {
                has_text = true;
                lines.push(payload.text);
            }
        }
    }
    if drop_list.is_empty() && !has_text {
        return false;
    }
    let Some(open) = Open::acquire(&WRITE_WAITS) else {
        host::log_info("clipboard", &[("result", "paste_clipboard_busy")]);
        return false;
    };
    if !win::empty(&open) {
        return false;
    }
    let mut wrote = false;
    if !drop_list.is_empty() {
        wrote |= win::set_paths(&open, &drop_list);
    }
    // Text for the apps that take text: the text items, with copied files as their paths. A burst of
    // files and images alone is the file list only.
    if has_text {
        wrote |= win::set_text(&open, &lines.join("\n"));
    }
    if !wrote {
        return false;
    }
    mark_own(open);
    true
}
