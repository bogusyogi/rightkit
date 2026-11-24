//! The two in-memory sessions of the device-approval key transfer ([`crate::transfer`]), one for each side, so a host does not
//! rebuild the bookkeeping around the four opaque messages.
//!
//! A host keeps one [`DeviceSessions`] (in a `static`, since a session spans several requests) and carries the messages between the
//! two devices over its authenticated device link. Nothing is persisted: a restart ends a session and the owner starts again. No
//! message and no code is logged here, and the key never leaves this crate unwrapped.
//!
//! * New device: [`DeviceSessions::request_begin`] (message 1), [`DeviceSessions::request_offer`] (takes message 2, returns message
//!   3 and the code to show), [`DeviceSessions::request_accept`] (the code typed from the approver's screen and message 4; installs
//!   the key).
//! * Approving device: [`DeviceSessions::approve_begin`], [`DeviceSessions::approve_reveal`], [`DeviceSessions::approve_show`],
//!   [`DeviceSessions::approve_commit`] (the owner typed the requester's code), [`DeviceSessions::approve_take_transfer`] (one use).

use crate::manager::KeyManager;
use crate::transfer::{Approver, Requester, TransferContext};
use std::sync::Mutex;

struct Session {
    /// The requesting device's id, as the device link named it.
    device: String,
    approver: Approver,
    /// Set once the owner typed the right code on the approving device.
    transfer: Option<Vec<u8>>,
}

/// The requester's and the approver's sessions of one device.
pub struct DeviceSessions {
    requester: Mutex<Option<Requester>>,
    approver: Mutex<Option<Session>>,
}

impl Default for DeviceSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceSessions {
    pub const fn new() -> Self {
        DeviceSessions {
            requester: Mutex::new(None),
            approver: Mutex::new(None),
        }
    }

    // ---- the new device's side ----------------------------------------------------------------------------

    /// Step 1 on the new device: the `Begin` message for the approver. Replaces any session in progress.
    pub fn request_begin(&self, context: TransferContext, label: &str) -> Result<Vec<u8>, String> {
        let (requester, begin) =
            Requester::begin(context, Some(label)).map_err(|e| e.to_string())?;
        *self.requester.lock().unwrap_or_else(|e| e.into_inner()) = Some(requester);
        Ok(begin)
    }

    /// Step 2 on the new device: takes the approver's `Offer`, returns the `Reveal` and the code to show on this screen.
    pub fn request_offer(&self, offer: &[u8]) -> Result<(Vec<u8>, String), String> {
        let mut guard = self.requester.lock().unwrap_or_else(|e| e.into_inner());
        let requester = guard
            .as_mut()
            .ok_or("no key request is in progress on this computer")?;
        let reveal = requester.receive_offer(offer).map_err(|e| e.to_string())?;
        let code = requester.code().map_err(|e| e.to_string())?;
        Ok((reveal, code))
    }

    /// Step 4 on the new device: the code typed from the approver's screen and its `Transfer`; installs the key through `manager`
    /// (asked for only once the code and transfer are good, as the install is the one effect).
    pub fn request_accept(
        &self,
        typed: &str,
        transfer: &[u8],
        manager: impl FnOnce() -> Result<KeyManager, String>,
    ) -> Result<(), String> {
        let mut guard = self.requester.lock().unwrap_or_else(|e| e.into_inner());
        let requester = guard
            .as_mut()
            .ok_or("no key request is in progress on this computer")?;
        let key = requester
            .accept(typed, transfer)
            .map_err(|e| e.to_string())?;
        manager()?.install(&key, false).map_err(|e| e.to_string())?;
        *guard = None;
        Ok(())
    }

    // ---- the approving device's side ----------------------------------------------------------------------

    /// Step 1 on the approving device: a `Begin` from `device`; returns the `Offer` and the requester's label. Replaces any session in
    /// progress.
    pub fn approve_begin(
        &self,
        context: TransferContext,
        device: &str,
        begin: &[u8],
    ) -> Result<(Vec<u8>, Option<String>), String> {
        let (approver, offer) =
            Approver::receive_begin(context, begin).map_err(|e| e.to_string())?;
        let label = approver.requester_label().map(str::to_owned);
        *self.approver.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
            device: device.to_owned(),
            approver,
            transfer: None,
        });
        Ok((offer, label))
    }

    /// Step 3 on the approving device: the requester's `Reveal`.
    pub fn approve_reveal(&self, device: &str, reveal: &[u8]) -> Result<(), String> {
        let mut guard = self.approver.lock().unwrap_or_else(|e| e.into_inner());
        let session = guard
            .as_mut()
            .filter(|s| s.device == device)
            .ok_or("no key request from that computer")?;
        session
            .approver
            .receive_reveal(reveal)
            .map_err(|e| e.to_string())
    }

    /// What the owner is shown on the approving device: the requesting device's name, its id and this screen's code.
    pub fn approve_show(&self) -> Result<(Option<String>, String, String), String> {
        let guard = self.approver.lock().unwrap_or_else(|e| e.into_inner());
        let session = guard.as_ref().ok_or("no computer is asking for the key")?;
        let code = session.approver.code().map_err(|e| e.to_string())?;
        Ok((
            session.approver.requester_label().map(str::to_owned),
            session.device.clone(),
            code,
        ))
    }

    /// The owner typed the requester's code here: wraps the current key of `manager`. The `Transfer` waits for the requester to ask.
    pub fn approve_commit(&self, typed: &str, manager: &KeyManager) -> Result<(), String> {
        let key = manager.require_current().map_err(|e| e.to_string())?;
        let mut guard = self.approver.lock().unwrap_or_else(|e| e.into_inner());
        let session = guard.as_mut().ok_or("no computer is asking for the key")?;
        session.transfer = Some(
            session
                .approver
                .approve(typed, &key)
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }

    /// The `Transfer`, once the owner approved, for the device it was made for; `None` until then. One use.
    pub fn approve_take_transfer(&self, device: &str) -> Option<Vec<u8>> {
        let mut guard = self.approver.lock().unwrap_or_else(|e| e.into_inner());
        let ready = guard
            .as_ref()
            .is_some_and(|s| s.device == device && s.transfer.is_some());
        if !ready {
            return None;
        }
        guard.take().and_then(|s| s.transfer)
    }
}
