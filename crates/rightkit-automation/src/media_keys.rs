//! Media and system keys: play/pause, next and previous track, volume up/down, mute.
//!
//! [`send_media_key`] posts one press (down then up) to the system, not to an app: whatever
//! owns the media session (Music, Spotify, a browser tab, the OS volume mixer) reacts as if
//! the keyboard key was pressed.
//!
//! * macOS: an `NSSystemDefined` event, subtype 8 (aux control buttons), built through
//!   `+[NSEvent otherEventWithType:...]` and posted to the HID tap with `CGEventPost`. The key
//!   type goes in `data1` bits 16 and up, the key state (0xA down, 0xB up) in bits 8 to 15.
//!   Needs no Accessibility grant to post on current macOS, but a sandboxed app may be refused.
//! * Windows: `VK_MEDIA_*` and `VK_VOLUME_*` through one `SendInput` call (down and up).
//!
//! Like the raw Windows input path this is an effect with no focus check and no receipt: it
//! only honours the process-wide kill switch ([`crate::policy::kill`]). A host with its own
//! admission hook should ask it before calling. The code tables are pure and tested on every
//! OS; the posting is not exercised by any test yet.

use crate::model::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKey {
    PlayPause,
    NextTrack,
    PreviousTrack,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaKey {
    pub const ALL: [MediaKey; 6] = [
        MediaKey::PlayPause,
        MediaKey::NextTrack,
        MediaKey::PreviousTrack,
        MediaKey::VolumeUp,
        MediaKey::VolumeDown,
        MediaKey::Mute,
    ];

    /// macOS `NX_KEYTYPE_*` value (IOKit `ev_keymap.h`): `SOUND_UP` 0, `SOUND_DOWN` 1,
    /// `MUTE` 7, `PLAY` 16, `NEXT` 17, `PREVIOUS` 18.
    pub fn mac_key_type(self) -> i64 {
        match self {
            MediaKey::VolumeUp => 0,
            MediaKey::VolumeDown => 1,
            MediaKey::Mute => 7,
            MediaKey::PlayPause => 16,
            MediaKey::NextTrack => 17,
            MediaKey::PreviousTrack => 18,
        }
    }

    /// Windows virtual-key code: `VK_VOLUME_MUTE` 0xAD, `VK_VOLUME_DOWN` 0xAE,
    /// `VK_VOLUME_UP` 0xAF, `VK_MEDIA_NEXT_TRACK` 0xB0, `VK_MEDIA_PREV_TRACK` 0xB1,
    /// `VK_MEDIA_PLAY_PAUSE` 0xB3.
    pub fn windows_vk(self) -> u16 {
        match self {
            MediaKey::Mute => 0xAD,
            MediaKey::VolumeDown => 0xAE,
            MediaKey::VolumeUp => 0xAF,
            MediaKey::NextTrack => 0xB0,
            MediaKey::PreviousTrack => 0xB1,
            MediaKey::PlayPause => 0xB3,
        }
    }

    /// Parse a name: `play-pause`, `next`, `previous` (also `prev`), `volume-up`,
    /// `volume-down`, `mute` (case-insensitive; `-`, `_` and space are interchangeable).
    pub fn parse(name: &str) -> Option<MediaKey> {
        let n = name.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Some(match n.as_str() {
            "play-pause" | "playpause" | "play" | "pause" => MediaKey::PlayPause,
            "next" | "next-track" | "nexttrack" => MediaKey::NextTrack,
            "previous" | "prev" | "previous-track" | "prev-track" | "previoustrack" => {
                MediaKey::PreviousTrack
            }
            "volume-up" | "volumeup" | "vol-up" => MediaKey::VolumeUp,
            "volume-down" | "volumedown" | "vol-down" => MediaKey::VolumeDown,
            "mute" | "volume-mute" => MediaKey::Mute,
            _ => return None,
        })
    }
}

/// `data1` of an `NSSystemDefined` subtype-8 event: key type in bits 16+, state in 8..16.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn aux_data1(key_type: i64, down: bool) -> i64 {
    let state: i64 = if down { 0xA } else { 0xB };
    (key_type << 16) | (state << 8)
}

/// Press and release `key` once, system-wide.
///
/// Fails with `Error::Blocked` while the kill switch is set, `Error::Unsupported` on an OS
/// without a backend, and `Error::Platform` / `Error::Unsupported` when the OS refuses.
pub fn send_media_key(key: MediaKey) -> Result<()> {
    if crate::policy::killed() {
        return Err(Error::Blocked("kill switch is set".into()));
    }
    send_impl(key)
}

#[cfg(target_os = "macos")]
fn send_impl(key: MediaKey) -> Result<()> {
    crate::macos::post_media_key(key.mac_key_type())
}

#[cfg(windows)]
fn send_impl(key: MediaKey) -> Result<()> {
    use crate::win::input::{send_input_batch, RawInput};
    send_input_batch(&RawInput::key_press(key.windows_vk()))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn send_impl(_key: MediaKey) -> Result<()> {
    Err(Error::Unsupported("media keys on this OS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_key_types_match_ev_keymap() {
        assert_eq!(MediaKey::VolumeUp.mac_key_type(), 0);
        assert_eq!(MediaKey::VolumeDown.mac_key_type(), 1);
        assert_eq!(MediaKey::Mute.mac_key_type(), 7);
        assert_eq!(MediaKey::PlayPause.mac_key_type(), 16);
        assert_eq!(MediaKey::NextTrack.mac_key_type(), 17);
        assert_eq!(MediaKey::PreviousTrack.mac_key_type(), 18);
    }

    #[test]
    fn windows_vks_match_winuser() {
        assert_eq!(MediaKey::Mute.windows_vk(), 0xAD);
        assert_eq!(MediaKey::VolumeDown.windows_vk(), 0xAE);
        assert_eq!(MediaKey::VolumeUp.windows_vk(), 0xAF);
        assert_eq!(MediaKey::NextTrack.windows_vk(), 0xB0);
        assert_eq!(MediaKey::PreviousTrack.windows_vk(), 0xB1);
        assert_eq!(MediaKey::PlayPause.windows_vk(), 0xB3);
    }

    #[test]
    fn codes_are_distinct() {
        for (i, a) in MediaKey::ALL.iter().enumerate() {
            for b in &MediaKey::ALL[i + 1..] {
                assert_ne!(a.mac_key_type(), b.mac_key_type());
                assert_ne!(a.windows_vk(), b.windows_vk());
            }
        }
    }

    #[test]
    fn aux_data1_layout() {
        assert_eq!(aux_data1(16, true), 0x0010_0A00);
        assert_eq!(aux_data1(16, false), 0x0010_0B00);
        assert_eq!(aux_data1(0, true), 0x0A00);
    }

    #[test]
    fn parse_names() {
        assert_eq!(MediaKey::parse("Play-Pause"), Some(MediaKey::PlayPause));
        assert_eq!(MediaKey::parse("prev"), Some(MediaKey::PreviousTrack));
        assert_eq!(MediaKey::parse("volume_up"), Some(MediaKey::VolumeUp));
        assert_eq!(MediaKey::parse("volume down"), Some(MediaKey::VolumeDown));
        assert_eq!(MediaKey::parse("mute"), Some(MediaKey::Mute));
        assert_eq!(MediaKey::parse("eject"), None);
    }
}
