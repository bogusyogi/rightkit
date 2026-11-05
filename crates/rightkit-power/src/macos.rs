//! macOS: sleep and wake from NSWorkspace, screen lock from the distributed notification
//! center, Low Power Mode from NSProcessInfo, and App Nap activities.
//!
//! The observers are registered with no queue, so each block runs on the thread that posts the
//! notification (the main thread for NSWorkspace). The tokens are removed when the reader drops.

#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::sel;
use objc2_app_kit::{
    NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSActivityOptions, NSDistributedNotificationCenter, NSNotification, NSNotificationCenter,
    NSNotificationName, NSObjectProtocol, NSProcessInfo, NSString,
};

use crate::{Ack, Event, Shared};

const SCREEN_LOCKED: &str = "com.apple.screenIsLocked";
const SCREEN_UNLOCKED: &str = "com.apple.screenIsUnlocked";

/// One registration: the center it was added to and the token `removeObserver:` takes.
type Observer = (
    Retained<NSNotificationCenter>,
    Retained<ProtocolObject<dyn NSObjectProtocol>>,
);

/// The registered observers, for as long as the `Power` lives.
pub(crate) struct Reader {
    observers: Vec<Observer>,
}

// SAFETY: the notification centers are thread-safe and the tokens are opaque objects only ever
// handed back to `removeObserver:` once, in `drop`; retaining and releasing is atomic.
#[allow(unsafe_code)]
unsafe impl Send for Reader {}
// SAFETY: as above; a shared reference gives no access to the centers or tokens.
#[allow(unsafe_code)]
unsafe impl Sync for Reader {}

impl Reader {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self, String> {
        read_low_power(&shared);
        let mut reader = Reader {
            observers: Vec::new(),
        };

        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: the two statics are Apple's published notification names.
        let (will_sleep, did_wake) = unsafe {
            (
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
            )
        };
        reader.observe(&workspace, will_sleep, Arc::clone(&shared), |shared| {
            shared.begin_suspending();
            shared.push(Event::Suspending(Ack::default()));
        });
        reader.observe(&workspace, did_wake, Arc::clone(&shared), |shared| {
            shared.push(Event::Resumed);
        });

        let distributed: Retained<NSNotificationCenter> =
            Retained::into_super(NSDistributedNotificationCenter::defaultCenter());
        let locked = NSString::from_str(SCREEN_LOCKED);
        let unlocked = NSString::from_str(SCREEN_UNLOCKED);
        reader.observe(&distributed, &locked, Arc::clone(&shared), |shared| {
            shared.update(|conditions| conditions.locked = true);
            shared.push(Event::Locked);
        });
        reader.observe(&distributed, &unlocked, shared, |shared| {
            shared.update(|conditions| conditions.locked = false);
            shared.push(Event::Unlocked);
        });
        Ok(reader)
    }

    fn observe(
        &mut self,
        center: &Retained<NSNotificationCenter>,
        name: &NSNotificationName,
        shared: Arc<Shared>,
        on_notification: impl Fn(&Shared) + 'static,
    ) {
        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
            on_notification(&shared);
        });
        // SAFETY: `name` is a notification name, the sender filter and queue are absent, and the
        // block only touches the `Arc<Shared>` it owns, which is thread-safe, so running it on
        // the posting thread is sound.
        #[allow(unsafe_code)]
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        self.observers.push((Retained::clone(center), token));
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        for (center, token) in self.observers.drain(..) {
            let observer: &AnyObject = (*token).as_ref();
            // SAFETY: `token` was returned by `addObserverForName:object:queue:usingBlock:` on
            // `center` and is removed exactly once, here.
            #[allow(unsafe_code)]
            unsafe {
                center.removeObserver(observer)
            };
        }
    }
}

/// Reads Low Power Mode. It arrived in macOS 12, where asking an older system would raise an
/// exception, so the selector is checked first.
pub(crate) fn read_low_power(shared: &Shared) {
    let info = NSProcessInfo::processInfo();
    if !info.respondsToSelector(sel!(isLowPowerModeEnabled)) {
        return;
    }
    let saver = info.isLowPowerModeEnabled();
    shared.update(|conditions| conditions.saver = saver);
}

/// An `NSProcessInfo` activity, ended when dropped.
pub(crate) struct Activity(Retained<ProtocolObject<dyn NSObjectProtocol>>);

// SAFETY: the token is an opaque object only ever handed back to `endActivity:`, once, in
// `drop`. NSProcessInfo is thread-safe and retain and release are atomic, so the token may be
// ended and released on any thread.
#[allow(unsafe_code)]
unsafe impl Send for Activity {}
// SAFETY: as above; a shared reference gives no access to the token.
#[allow(unsafe_code)]
unsafe impl Sync for Activity {}

impl Activity {
    pub(crate) fn begin(reason: &str) -> Self {
        Self(
            NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str(reason),
            ),
        )
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: the token came from `beginActivityWithOptions:reason:` and is ended exactly
        // once, here, before its reference is released.
        #[allow(unsafe_code)]
        unsafe {
            NSProcessInfo::processInfo().endActivity(&self.0)
        };
    }
}
