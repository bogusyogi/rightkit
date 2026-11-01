//! Clipboard / pasteboard: plain and promised text staging, snapshot/restore,
//! and receipt tracking for delivery confirmation.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSObject, NSString};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub fn set_clipboard(text: &str) -> bool {
    let pb = NSPasteboard::generalPasteboard();
    let expected_change_count = pb.changeCount() as i64;
    stage_clipboard_text(text, expected_change_count).is_ok()
}

/// Stage text & return the exact pasteboard generation created by the app's
/// `clearContents`. AppKit keeps writes following that declaration in the same
/// generation, so later restore checks cannot adopt a concurrent writer's
/// generation by reading it after this function returns.
pub fn stage_clipboard_text(text: &str, expected_change_count: i64) -> Result<i64, Option<i64>> {
    let pb = NSPasteboard::generalPasteboard();
    if pb.changeCount() as i64 != expected_change_count {
        return Err(None);
    }
    let transcript_change_count = pb.clearContents() as i64;
    let s = NSString::from_str(text);
    let ty = unsafe { NSPasteboardTypeString };
    if pb.setString_forType(&s, ty) {
        Ok(transcript_change_count)
    } else {
        Err(Some(transcript_change_count))
    }
}

/// One complete, eager copy of one pasteboard item. Keeping every declared
/// type matters: a temporary Cmd+C must not silently discard rich text,
/// links, images, or file promises when the user's clipboard is restored.
#[derive(Clone, Debug)]
struct PasteboardItemSnapshot {
    representations: Vec<(String, Vec<u8>)>,
}

/// Eager snapshot used by the selected-text copy fallback. `change_count` is
/// the AppKit generation observed before we post Cmd+C; callers must verify a
/// later generation before restoring so a concurrent clipboard write wins.
#[derive(Clone, Debug)]
pub struct PasteboardSnapshot {
    change_count: i64,
    items: Vec<PasteboardItemSnapshot>,
}

impl PasteboardSnapshot {
    pub fn change_count(&self) -> i64 {
        self.change_count
    }
}

pub fn pasteboard_change_count() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}

/// Serialize app-owned clipboard transactions in this process.
///
/// AppKit exposes a generation counter, but no compare-and-declare operation.
/// Holding this lease across snapshot, staging, dispatch, and restoration
/// closes the race between the app's own delivery/copy paths. External
/// writers are observed by generation checks at each destructive step; AppKit
/// still leaves an unavoidable inter-process check/commit window.
pub fn acquire_clipboard_transaction() -> MutexGuard<'static, ()> {
    static LEASE: OnceLock<Mutex<()>> = OnceLock::new();
    LEASE
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn data_bytes(data: &NSData) -> Option<Vec<u8>> {
    let length = data.length();
    let mut bytes = vec![0u8; length];
    if length == 0 {
        return Some(bytes);
    }
    let pointer = std::ptr::NonNull::new(bytes.as_mut_ptr().cast())?;
    // SAFETY: `pointer` points to `length` writable bytes owned by `bytes` and
    // remains valid for the duration of this synchronous AppKit call.
    unsafe { data.getBytes_length(pointer, length as _) };
    Some(bytes)
}

/// Capture all current pasteboard items and their declared representations.
/// If AppKit cannot materialize any representation, return `None` so callers
/// can leave the user's clipboard untouched rather than restore a lossy copy.
fn snapshot_pasteboard(pasteboard: &NSPasteboard) -> Option<PasteboardSnapshot> {
    let change_count = pasteboard.changeCount() as i64;
    let Some(items) = pasteboard.pasteboardItems() else {
        // An empty pasteboard has no items to materialize, but is still a
        // complete restorable state.
        return (pasteboard.changeCount() as i64 == change_count).then_some(PasteboardSnapshot {
            change_count,
            items: Vec::new(),
        });
    };
    let mut snapshot_items = Vec::with_capacity(items.count());
    for index in 0..items.count() {
        let item = items.objectAtIndex(index);
        let types = item.types();
        let mut representations = Vec::with_capacity(types.count());
        for type_name in types.iter() {
            let data = item.dataForType(&type_name)?;
            representations.push((type_name.to_string(), data_bytes(&data)?));
        }
        snapshot_items.push(PasteboardItemSnapshot { representations });
    }
    (pasteboard.changeCount() as i64 == change_count).then_some(PasteboardSnapshot {
        change_count,
        items: snapshot_items,
    })
}

pub fn snapshot_general_pasteboard() -> Option<PasteboardSnapshot> {
    snapshot_pasteboard(&NSPasteboard::generalPasteboard())
}

/// Restore a complete pasteboard snapshot only when its current generation is
/// still `expected_change_count`. A changed generation means another process
/// wrote the clipboard while Cmd+C was in flight; in that case do nothing.
fn restore_pasteboard_if_unchanged(
    pasteboard: &NSPasteboard,
    snapshot: PasteboardSnapshot,
    expected_change_count: i64,
) -> bool {
    if pasteboard.changeCount() as i64 != expected_change_count {
        return false;
    }

    let mut objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
    for item_snapshot in snapshot.items {
        let item = NSPasteboardItem::new();
        for (type_name, bytes) in item_snapshot.representations {
            let type_name = NSString::from_str(&type_name);
            let data = NSData::from_vec(bytes);
            if !item.setData_forType(&data, &type_name) {
                return false;
            }
        }
        objects.push(ProtocolObject::from_retained(item));
    }

    // Check immediately before the destructive operation as well. This keeps
    // the normal inter-process write window bounded; AppKit offers no atomic
    // compare-and-clear, so a writer racing this final check remains an OS
    // limitation rather than something this function can claim to prevent.
    if pasteboard.changeCount() as i64 != expected_change_count {
        return false;
    }
    pasteboard.clearContents();
    if objects.is_empty() {
        return true;
    }
    let object_refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
        objects.iter().map(|object| &**object).collect();
    let objects = NSArray::from_slice(&object_refs);
    pasteboard.writeObjects(&objects)
}

pub fn restore_general_pasteboard_if_unchanged(
    snapshot: &PasteboardSnapshot,
    expected_change_count: i64,
) -> bool {
    restore_general_pasteboard_if_unchanged_owned(snapshot.clone(), expected_change_count)
}

/// Owned restore path used by delivery transactions. Consuming the snapshot
/// lets NSData take each captured representation's bytes without a second
/// per-format Vec clone.
pub fn restore_general_pasteboard_if_unchanged_owned(
    snapshot: PasteboardSnapshot,
    expected_change_count: i64,
) -> bool {
    restore_pasteboard_if_unchanged(
        &NSPasteboard::generalPasteboard(),
        snapshot,
        expected_change_count,
    )
}

/// State shared by a promised pasteboard owner and its delivery waiter.
///
/// AppKit may ask for promised data before the paste keystroke reaches its
/// target (clipboard managers commonly do this). Such reads are retained for
/// diagnostics but only reads at or after `posted_at` count as delivery proof.
struct PromisedPasteboardState {
    posted_at: Option<Instant>,
    receipts: Vec<Instant>,
    ownership_lost: bool,
}

impl PromisedPasteboardState {
    fn new() -> Self {
        Self {
            posted_at: None,
            receipts: Vec::new(),
            ownership_lost: false,
        }
    }

    fn valid_receipts(&self) -> impl Iterator<Item = &Instant> {
        self.receipts
            .iter()
            .filter(move |at| self.posted_at.is_some_and(|posted| **at >= posted))
    }
}

pub struct PromisedPasteboardProviderIvars {
    state: Arc<(Mutex<PromisedPasteboardState>, Condvar)>,
    text: String,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Rust ivars are owned
    // values, and callback state is guarded by a Mutex.
    #[unsafe(super(NSObject))]
    #[name = "RightKitDesktopPasteboardProvider"]
    #[ivars = PromisedPasteboardProviderIvars]
    pub struct PromisedPasteboardProvider;

    impl PromisedPasteboardProvider {
        /// NSPasteboardOwner callback: this is the exact moment a consumer
        /// requested our promised text from AppKit.
        #[unsafe(method(pasteboard:provideDataForType:))]
        fn pasteboard_provide_data_for_type(
            &self,
            pasteboard: &NSPasteboard,
            data_type: &NSString,
        ) {
            let ivars = self.ivars();
            let is_text = unsafe { data_type.isEqualToString(NSPasteboardTypeString) };
            if is_text {
                let (lock, wake) = &*ivars.state;
                if let Ok(mut state) = lock.lock() {
                    state.receipts.push(Instant::now());
                    wake.notify_all();
                }
            }

            // Only text is a useful delivery payload. Declared auxiliary
            // marker types are answered with an empty value so they cannot be
            // mistaken for receipts or expose transcript contents.
            let payload = if is_text {
                NSString::from_str(&ivars.text)
            } else {
                NSString::from_str("")
            };
            let _ = pasteboard.setString_forType(&payload, data_type);
        }

        #[unsafe(method(pasteboardChangedOwner:))]
        fn pasteboard_changed_owner(&self, _pasteboard: &NSPasteboard) {
            let (lock, wake) = &*self.ivars().state;
            if let Ok(mut state) = lock.lock() {
                state.ownership_lost = true;
                wake.notify_all();
            }
        }
    }
);

impl PromisedPasteboardProvider {
    fn new(state: Arc<(Mutex<PromisedPasteboardState>, Condvar)>, text: String) -> Retained<Self> {
        let this = Self::alloc().set_ivars(PromisedPasteboardProviderIvars { state, text });
        unsafe { msg_send![super(this), init] }
    }
}

/// Receipt observed while waiting for a promised pasteboard read.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PasteboardReceipt {
    /// Number of text reads at or after [`PromisedPasteboard::mark_posted`].
    pub count: usize,
    /// First valid read timestamp, if a target requested the text.
    pub first_at: Option<Instant>,
    /// Most recent valid read timestamp, if a target requested the text.
    pub last_at: Option<Instant>,
    /// Whether another process replaced our pasteboard declaration.
    pub ownership_lost: bool,
}

impl PasteboardReceipt {
    pub fn observed(self) -> bool {
        self.count > 0
    }
}

/// A live promised pasteboard declaration.
///
/// Create this on the thread that owns AppKit's run loop, call
/// [`mark_posted`](Self::mark_posted) immediately before posting Cmd+V, then
/// wait on [`wait_for_receipt`](Self::wait_for_receipt) from a worker. Keep
/// this value alive until [`restore_if_unchanged`](Self::restore_if_unchanged)
/// has completed; dropping it without restoring intentionally leaves the
/// transcript declaration untouched rather than risking a stale overwrite.
pub struct PromisedPasteboard {
    state: Arc<(Mutex<PromisedPasteboardState>, Condvar)>,
    change_count: i64,
    provider: Option<Retained<PromisedPasteboardProvider>>,
}

/// Publish UTF-8 text as an AppKit promised pasteboard value.
///
/// `expected_change_count` is checked before declaration, matching
/// [`stage_clipboard_text`]. `Err(None)` means another process changed the
/// clipboard first; `Err(Some(count))` means declaration itself failed.
pub fn stage_promised_clipboard_text(
    text: &str,
    expected_change_count: i64,
) -> Result<PromisedPasteboard, Option<i64>> {
    let pasteboard = NSPasteboard::generalPasteboard();
    if pasteboard.changeCount() as i64 != expected_change_count {
        return Err(None);
    }

    let state = Arc::new((Mutex::new(PromisedPasteboardState::new()), Condvar::new()));
    let provider = PromisedPasteboardProvider::new(state.clone(), text.to_string());
    let types = NSArray::from_retained_slice(&[NSString::from_str("public.utf8-plain-text")]);
    // declareTypes:owner: clears current contents and returns this declaration
    // generation. The provider is retained in the transaction as well as by
    // AppKit so callback state remains alive until explicit restoration.
    let change_count: i64 =
        unsafe { msg_send![&*pasteboard, declareTypes: &*types, owner: &*provider] };
    if change_count <= 0 {
        return Err(Some(change_count));
    }

    Ok(PromisedPasteboard {
        state,
        change_count,
        provider: Some(provider),
    })
}

impl PromisedPasteboard {
    /// Mark exact paste-chord injection time. Receipts before this call are
    /// ignored as third-party/eager reads.
    pub fn mark_posted(&self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.posted_at = Some(Instant::now());
            wake.notify_all();
        }
    }

    /// Return receipt evidence without waiting.
    pub fn receipt(&self) -> PasteboardReceipt {
        let (lock, _) = &*self.state;
        let Ok(state) = lock.lock() else {
            return PasteboardReceipt {
                ownership_lost: true,
                ..PasteboardReceipt::default()
            };
        };
        let mut valid = state.valid_receipts();
        let first_at = valid.next().copied();
        let last_at = state.valid_receipts().last().copied();
        PasteboardReceipt {
            count: state.valid_receipts().count(),
            first_at,
            last_at,
            ownership_lost: state.ownership_lost,
        }
    }

    /// Wait until a valid receipt, ownership loss, or timeout. This uses a
    /// condition variable, so worker waits do not poll or block AppKit's run
    /// loop that services `provideDataForType:`.
    pub fn wait_for_receipt(&self, timeout: Duration) -> PasteboardReceipt {
        let (lock, wake) = &*self.state;
        let Ok(mut state) = lock.lock() else {
            return PasteboardReceipt {
                ownership_lost: true,
                ..PasteboardReceipt::default()
            };
        };
        let deadline = Instant::now() + timeout;
        loop {
            if state.valid_receipts().next().is_some() || state.ownership_lost {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok((next, _)) = wake.wait_timeout(state, remaining) else {
                return PasteboardReceipt {
                    ownership_lost: true,
                    ..PasteboardReceipt::default()
                };
            };
            state = next;
        }
        let mut valid = state.valid_receipts();
        let first_at = valid.next().copied();
        let last_at = state.valid_receipts().last().copied();
        PasteboardReceipt {
            count: state.valid_receipts().count(),
            first_at,
            last_at,
            ownership_lost: state.ownership_lost,
        }
    }

    pub fn change_count(&self) -> i64 {
        self.change_count
    }

    /// Restore prior contents only while declaration still owns the current
    /// pasteboard generation. Caller should invoke this after its bounded
    /// receipt wait; changed ownership always wins over restoration.
    pub fn restore_if_unchanged(&mut self, snapshot: &PasteboardSnapshot) -> bool {
        self.restore_if_unchanged_owned(snapshot.clone())
    }

    /// Owned restore path for a one-shot delivery transaction. This avoids
    /// cloning every captured representation before rebuilding AppKit items.
    pub fn restore_if_unchanged_owned(&mut self, snapshot: PasteboardSnapshot) -> bool {
        let (lock, _) = &*self.state;
        if lock
            .lock()
            .map(|state| state.ownership_lost)
            .unwrap_or(true)
        {
            return false;
        }
        let restored = restore_general_pasteboard_if_unchanged_owned(snapshot, self.change_count);
        if restored {
            self.provider = None;
        }
        restored
    }
}

pub fn get_clipboard() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    let ty = unsafe { NSPasteboardTypeString };
    pb.stringForType(ty).map(|s| s.to_string())
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
