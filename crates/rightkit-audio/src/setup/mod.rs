//! Setup-flow audio (feature `capture`): the live microphone level meter, device probing, the
//! one-shot calibration capture an onboarding or setup step needs, the OS microphone consent
//! flows, and the calibration engine child run.
//!
//! * [`mic_meter`]: a level meter with an epoch-owned lifecycle (a device change replaces an
//!   active meter, a stale capture thread can never stop its successor), a first-buffer health
//!   probe, one repair reopen, and a dB-scale level. Events leave through a host callback.
//! * [`calibration_capture`]: a single-slot recorder that collects mono PCM for a calibration
//!   pass, with a hard length ceiling, session ownership checks, and superseding of a capture
//!   left by a session that no longer exists.
//! * [`media_guard`]: restore-once guard around a host's "pause other media" calls.
//! * [`permissions`]: microphone consent status and request per OS (macOS `AVCaptureDevice`,
//!   Windows `AppCapability` plus a capture probe), and the macOS Settings opener.
//! * [`calibration_engine`]: runs a host's recognition engine as a bounded child over a
//!   retained PCM file and decodes its JSON result.
//! * [`calibration_session`]: finishes capture, bounds PCM lifetime with an owned temp,
//!   and coordinates host model, engine and restart hooks.
//!
//! Nothing here conditions or analyses audio for recognition: the host keeps its own conditioning
//! authority and decides what captured PCM is used for. Renderer events, copy, paths & thresholds
//! are host inputs; diagnostic event identifiers remain stable for existing consumers.
//!
//! Constructors never open devices. Hosts call `start` only after an explicit user action &
//! verifying an existing OS microphone grant (see [`permissions`]).
//! Stop metering & cancel recording when setup exits or its window hides, suspends or closes.

pub mod calibration_capture;
pub mod calibration_engine;
pub mod calibration_session;
pub mod media_guard;
pub mod mic_meter;
pub mod permissions;

/// Setup audio admission, suspend/resume & explicit off-UI teardown.
pub mod scope;
