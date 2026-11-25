//! The composed clip-card flows: show the card (overlay surface and native-panel surface), close it
//! on request, and release the app-side holds when the pill reports the card dismissed. The
//! override-slot rules are `clip_override`'s; this module is the order in which a host calls them.
//!
//! What the host supplies: the rows (`items_for`: the override transcript alone, else the newest
//! history rows, read from the host's history), the calls that show or close the card on its
//! surface, the release of its own holds, the sink and, for a native panel, the wire names and
//! numbers of its payload. The crate never reads a history, a setting or a window.
//!
//! Moved from HeardRight `pill_overlays.rs` (`show_pill_clipboard` on both platforms,
//! `dismiss_pill_clipboard_request`, `release_pill_clipboard_holds`). Event names, ordering and
//! error text are unchanged.

use crate::clip_override::{clear_commit, clear_if_current, commit, set_while_showing, ClipSource};
use crate::clipboard::ClipItem;
use crate::host_glue::Sink;
use crate::ClipRefusal;

/// Show the clip card on an overlay surface. `transcript` is the override that selects the single
/// card (`None`: the recent list). `items_for(transcript)` reads the rows for exactly the override
/// this call asks for, never the one still in the slot: the previous override is not what the card
/// will show. With no rows nothing opens: `dismiss` runs (the override a single card would have
/// set is dropped with it) and the call succeeds. Otherwise the override is written while the card
/// is shown (`clip_override::set_while_showing`): it is kept only when `show` accepted the card,
/// and a refused or dropped card restores what was there before. `show(items, single, generation)`
/// receives the generation of that write, which travels with the card so the pill's owner thread
/// ends on the newer of two shows that reach it out of order. A refusal is returned as
/// `pill clipboard not shown: <code>`: a stable tag, never the transcript or an error string.
/// Logs `pill_clipboard_open` (`path=native single=<bool> count=<n>`) before showing.
pub fn show_overlay(
    log: Sink,
    transcript: Option<String>,
    items_for: impl FnOnce(Option<String>) -> Result<Vec<ClipSource>, String>,
    dismiss: impl FnOnce(),
    show: impl FnOnce(Vec<ClipItem>, bool, u64) -> Result<(), ClipRefusal>,
) -> Result<(), String> {
    let single = transcript.is_some();
    let items = items_for(transcript.clone())?
        .into_iter()
        .map(|row| ClipItem {
            id: row.id,
            primary: row.primary,
            alternate: row.alternate,
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        dismiss();
        return Ok(());
    }
    let shown = set_while_showing(transcript, |generation| {
        log(
            "pill_clipboard_open",
            &format!("path=native single={single} count={}", items.len()),
        );
        show(items, single, generation)
    })?;
    shown.map_err(|refusal| format!("pill clipboard not shown: {}", refusal.code()))
}

/// The host dismisses the clip card: release the host-side holds (`set_holds(false)`: the "pill
/// stays expanded" flag), clear the override transcript that selects the single card and close the
/// card with the generation of the clear (always fresh: the shared clear recovers a poisoned slot),
/// so the pill's owner thread drops the close when a newer show was applied first.
pub fn dismiss_overlay(set_holds: impl FnOnce(bool), close: impl FnOnce(u64)) {
    set_holds(false);
    let generation = clear_commit();
    close(generation);
}

/// The pill dismissed the card made by the show with `generation` (click outside, the card's own
/// close): `release` runs only while that card is still the newest request
/// (`clip_override::clear_if_current` compares under the slot's lock, and an unreadable slot
/// releases too); a dismissal of a card that a newer show has replaced releases nothing. Called on
/// the pill's owner thread.
pub fn release_holds(generation: u64, release: impl FnOnce()) {
    match clear_if_current(generation) {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => release(),
    }
}

/// How a native panel's clip payload is written: the JSON key of each field and the mode numbers
/// of its wire (a single card, the recent list).
#[derive(Clone, Copy, Debug)]
pub struct PanelWire<'a> {
    pub id_key: &'a str,
    pub primary_key: &'a str,
    pub alternate_key: &'a str,
    pub single_mode: i32,
    pub recent_mode: i32,
}

fn json_string(text: &str) -> Result<String, String> {
    serde_json::to_string(text).map_err(|err| format!("serialize native pill clipboard: {err}"))
}

/// The JSON array a native panel receives: one object per row with the keys of `wire`, in the
/// order id, primary, alternate (`null` when there is none).
pub fn panel_payload(items: &[ClipSource], wire: &PanelWire<'_>) -> Result<String, String> {
    let mut out = String::from("[");
    for (index, row) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('{');
        out.push_str(&json_string(wire.id_key)?);
        out.push(':');
        out.push_str(&json_string(&row.id)?);
        out.push(',');
        out.push_str(&json_string(wire.primary_key)?);
        out.push(':');
        out.push_str(&json_string(&row.primary)?);
        out.push(',');
        out.push_str(&json_string(wire.alternate_key)?);
        out.push(':');
        match &row.alternate {
            Some(alternate) => out.push_str(&json_string(alternate)?),
            None => out.push_str("null"),
        }
        out.push('}');
    }
    out.push(']');
    Ok(out)
}

/// Show the clip card on a native panel. The rows and the mode come from the request
/// (`items_for(transcript)`, `wire.single_mode` for an override and `wire.recent_mode` otherwise),
/// not from the override the slot held before it. The override is committed
/// (`clip_override::commit`) and its generation travels with the show: two shows that race end on
/// the one whose override was committed last, whichever reaches the view first. With no rows the
/// card is closed (`close(generation)`); otherwise `set_items(payload, mode, generation)` shows it
/// and, when it reports the view accepted it, `pill_clipboard_show` (`path=native mode=<n>
/// count=<n>`) is logged.
pub fn show_panel(
    log: Sink,
    transcript: Option<String>,
    items_for: impl FnOnce(Option<String>) -> Result<Vec<ClipSource>, String>,
    wire: &PanelWire<'_>,
    close: impl FnOnce(u64),
    set_items: impl FnOnce(&str, i32, u64) -> bool,
) -> Result<(), String> {
    let mode = if transcript.is_some() {
        wire.single_mode
    } else {
        wire.recent_mode
    };
    let items = items_for(transcript.clone())?;
    let generation = commit(transcript)?;

    if items.is_empty() {
        close(generation);
        return Ok(());
    }
    let payload = panel_payload(&items, wire)?;
    if set_items(&payload, mode, generation) {
        log(
            "pill_clipboard_show",
            &format!("path=native mode={mode} count={}", items.len()),
        );
    }
    Ok(())
}
