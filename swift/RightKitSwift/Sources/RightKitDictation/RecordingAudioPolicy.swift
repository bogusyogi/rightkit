#if os(iOS)
@preconcurrency import AVFoundation
import Foundation
#if canImport(UIKit)
import UIKit
#endif

/// Owns the recording audio-session policy: whether dictation ducks/stops
/// other audio (`MixPolicy`), and whether a connected Bluetooth microphone
/// gets iOS 26's high-quality recording mode. Both are internal dogfood
/// knobs, not customer-facing settings — there is no Settings UI for either.
///
/// The product default is non-mixing (music stops while dictating); a
/// deep link (`heardright://dogfood-audio-mix?policy=mix`, wired in
/// `DictationShortcuts.handleDeepLink`) flips a device to the old
/// mix-with-others behavior for A/B comparison without a rebuild.
@MainActor
enum RecordingAudioPolicy {
    // Internal TestFlight comparison: August 30 session settings on today's
    // capture/recovery implementation. Ignore persisted experiment knobs so
    // both testers exercise the same configuration. Snapshot: b7bd7233.
    static let usesAugust30AudioSettings = false
    // Device testing rejected Voice I/O + voiceChat: Bluetooth capture was
    // either unintelligible or failed kAUStartIO. Keep 0904 options/retries.
    nonisolated static let usesVoiceProcessingExperiment = false

    enum MixPolicy: String {
        case nonMixing = "non_mixing"
        case mixWithOthers = "mix_with_others"
        /// Mix only when the app is NOT active at start time (background
        /// Action Button / Shortcut starts), so background activation is
        /// permitted; a foreground start stays non-mixing (music stops).
        /// The shipping default: see `baseOptions(for:appActive:)` for the
        /// foreground/background option matrix.
        ///
        /// The `mixThenInterrupt` and `mixInterruptAfterCapture` diagnostic
        /// arms are retired: both re-`setCategory` on a live session, which
        /// added a whole extra state machine (plus a self-interrupt
        /// suppression window) to the most fragile path for no shippable
        /// gain. A device still pinned to one of their retired raw values
        /// decodes to `nil` and falls back to this default automatically.
        case backgroundMixOnly = "background_mix_only"
        /// Background starts use the one PUBLIC option that is still
        /// mixable-class (so background activation is permitted) yet acts on
        /// other audio: `.interruptSpokenAudioAndMixWithOthers` pauses
        /// spoken-audio apps (podcasts, audiobooks, spoken-audio mode) while
        /// music keeps mixing; they resume on `notifyOthersOnDeactivation`.
        /// A separate experiment adds background-only ducking. Foreground
        /// starts stay non-mixing (music stops). Universal pause needs the
        /// foreground, which iOS refuses to grant an Action Button intent
        /// (docs/results/2026-09-06-ios-foreground-for-media.md).
        case backgroundInterruptSpoken = "background_interrupt_spoken"
        case backgroundDuck = "background_duck"
        case backgroundDuckAndSpoken = "background_duck_and_spoken"
    }

    /// Which audio-session SHAPE a start uses. A temporary dogfood A/B knob
    /// for the Bose regression: transcription quality and
    /// background engine start were both physically good on the b7bd7233
    /// session shape and are not on the current one, so the deltas have to be
    /// isolated empirically rather than argued about.
    ///
    /// Every arm keeps ALL of the current crash/lifecycle safeguards (single
    /// activation, single retry owner, fresh `AVAudioEngine` per attempt,
    /// Live Activity handling, full telemetry). Only the session shape moves.
    /// Selected with `heardright://dogfood-audio-profile?profile=…`.
    ///
    /// DELETE this enum and pin the winner once the A/B has a verdict.
    enum SessionProfile: String {
        /// Build 2026090209 as shipped: mode `.default`, iOS 26 HQ Bluetooth
        /// requested with HFP fallback, explicit `setPreferredInput`.
        case current

        /// The b7bd7233 session shape that was physically good with this
        /// exact Bose headset: mode `.spokenAudio`, always `.mixWithOthers`,
        /// `.allowBluetoothHFP` only, and NO explicit input pin — HFP is
        /// enabled and iOS is left to choose the route.
        case legacyBose = "legacy_bose"

        /// Factor A — legacy arm, but mode `.default`. Isolates the mode.
        case legacyDefaultMode = "legacy_default_mode"

        /// Factor B — legacy arm, but with the explicit `setPreferredInput`
        /// pin restored. Isolates the post-activation route transition that
        /// is the leading background-`kAUStartIO` regression candidate.
        case legacyPreferredInput = "legacy_preferred_input"

        /// Factor B(nonmix) — legacy arm, but foreground stays NON-mixable
        /// (music stops), which is the current product behavior. Isolates the
        /// always-mix → foreground-nonmix change, the least-examined delta
        /// and a plausible cause of a routing/arbitration difference that
        /// leaves the reported port type as HFP while changing what the
        /// microphone actually delivers.
        case legacyForegroundNonmix = "legacy_foreground_nonmix"

        /// Factor C — legacy arm, but requesting
        /// `.bluetoothHighQualityRecording` with the documented HFP fallback.
        case legacyHQ = "legacy_hq"

        /// `.spokenAudio` on every legacy-derived arm except factor A.
        ///
        /// Apple documents `.spokenAudio` as a PLAYBACK-oriented mode
        /// ("podcast players and audio books"), so on the label alone
        /// `.default` is the cleaner choice — which is exactly why it was
        /// changed. But the label is not evidence: the `.spokenAudio` shape
        /// is the one that physically produced good Bose transcription, and
        /// modes can alter specialized session behavior. Factor A exists to
        /// settle it with device data instead of documentation reading.
        var mode: AVAudioSession.Mode {
            switch self {
            case .current, .legacyDefaultMode:
                return .default
            case .legacyBose, .legacyPreferredInput, .legacyHQ, .legacyForegroundNonmix:
                return .spokenAudio
            }
        }

        /// Whether to compose in `.bluetoothHighQualityRecording` (iOS 26+).
        var requestsHighQualityBluetooth: Bool {
            switch self {
            case .current, .legacyHQ:
                return true
            case .legacyBose, .legacyDefaultMode, .legacyPreferredInput, .legacyForegroundNonmix:
                return false
            }
        }

        /// Whether to call `setPreferredInput` for a Bluetooth port after
        /// activation. The legacy arms do NOT: the physically working code
        /// never pinned an input, and pinning one on an already-active
        /// session can start a route transition whose `currentRoute` reports
        /// HFP before the I/O path will accept a new `AVAudioEngine`.
        var pinsPreferredBluetoothInput: Bool {
            switch self {
            case .current, .legacyPreferredInput:
                return true
            case .legacyBose, .legacyDefaultMode, .legacyHQ, .legacyForegroundNonmix:
                return false
            }
        }

        /// The legacy arms always carry `.mixWithOthers`, foreground included
        /// — that is part of the shape being reproduced, and it is what the
        /// `MixPolicy` knob would otherwise only grant to background starts.
        var forcesMixWithOthers: Bool {
            switch self {
            case .current, .legacyForegroundNonmix:
                return false
            case .legacyBose, .legacyDefaultMode, .legacyPreferredInput, .legacyHQ:
                return true
            }
        }
    }

    /// How long a background-originated start keeps retrying `AVAudioEngine.start()`.
    ///
    /// The one background start in our telemetry that ever succeeded
    /// failed `kAUStartIO` on attempts 1 and 2 and
    /// only got through on attempt 3 — under a ladder whose outer and inner
    /// schedules compounded, so its third attempt fired far later than the
    /// current 700 ms cap allows. Tightening that schedule to an honest
    /// absolute ladder may have closed the window HFP needs.
    ///
    /// `long` exists to measure two unknowns at once: whether the engine ever
    /// starts on an HFP route from the background given seconds rather than
    /// milliseconds, and how long iOS actually tolerates before its
    /// undocumented `AudioRecordingIntent` watchdog kills the intent. If the
    /// watchdog fires first the intent dies and the system shows its own
    /// alert — that outcome is itself the measurement.
    ///
    /// DELETE once the background-Bluetooth question has an answer.
    enum RetryLadder: String {
        /// Shipping: `[0, 350, 700]` ms for system-originated starts.
        case standard
        /// Reproduces the background engine-attempt timing used on August 30.
        case august30 = "august_30"
        /// Diagnostic: extends well past any plausible route-settle time.
        case long

        /// Absolute offsets in ms from the start of the attempt sequence.
        func offsetsMs(inAppButton: Bool) -> [UInt64] {
            if inAppButton { return [0, 350, 750, 1250] }
            switch self {
            case .standard:
                return [0, 350, 700]
            case .august30:
                return [0, 150, 300, 450]
            case .long:
                return [0, 350, 700, 1500, 3000, 5000, 8000]
            }
        }
    }

    /// What a background retry does BETWEEN failed `AVAudioEngine.start()`
    /// attempts. Today it does nothing: the ladder only rebuilds the session
    /// when the SESSION failed, and here the session activates cleanly and
    /// only the engine refuses, so all seven rungs retry against the exact
    /// same session and the same route. That is untested ground.
    ///
    /// DELETE once the background-Bluetooth question has an answer.
    enum EngineRecovery: String {
        /// Shipping: retry the engine against the already-active session.
        case sameSession = "same_session"
        /// Deactivate and reactivate the session before each retry, so the
        /// engine is offered a genuinely new session rather than the one it
        /// has already refused.
        case rebuildSession = "rebuild_session"
        /// Wait for an `AVAudioSession.routeChangeNotification` instead of
        /// sleeping to the next offset. If the real blocker is a route
        /// transition in flight (e.g. A2DP → HFP), the route change IS the
        /// ready signal and a timer can only ever guess at it.
        case awaitRouteChange = "await_route_change"
        /// Both: wait for a route change AND rebuild the session.
        case rebuildAndAwaitRoute = "rebuild_and_await_route"

        var rebuildsSession: Bool {
            self == .rebuildSession || self == .rebuildAndAwaitRoute
        }

        var awaitsRouteChange: Bool {
            self == .awaitRouteChange || self == .rebuildAndAwaitRoute
        }
    }

    /// Whether to switch the session to non-mixable AFTER it is active, so a
    /// background start can activate mixably (which is what `!int` 560557684
    /// demands) and still stop other audio.
    ///
    /// This is the restored `mix_then_interrupt` arm. Its device
    /// evidence: it DID fix background activation, and then produced
    /// `record_capture_unreadable` on 3 of 4 runs because re-`setCategory` on
    /// a live session interrupts our own session. It is restored as a
    /// CONTROL for the current `kAUStartIO` failure — which happens after
    /// activation, so this arm is not expected to fix it — and this time the
    /// self-interrupt suppression window comes back with it.
    ///
    /// DELETE with the rest of the diagnostic scaffolding.
    enum MixSwitch: String {
        /// Shipping: never re-`setCategory` a live session.
        case none
        /// The OLD arm: drop `.mixWithOthers` once capture is running, while
        /// STAYING `.playAndRecord`. Distinct from the two `.record` arms —
        /// it changes options, not the category.
        case interruptAfterCapture = "interrupt_after_capture"

        /// Background control: remove mixing options before starting I/O,
        /// preserving category, mode & routing options. Compare against the
        /// failed `.record` request without relying on its partial side effects.
        case nonmixBeforeEngine = "nonmix_before_engine"
        // 23B: preserve mixability; change only ducking before engine start.
        case unduckBeforeEngine = "unduck_before_engine"

        /// **R1.** Activate mixably (which is what background activation
        /// requires — a non-mixable `.playAndRecord` activated in the
        /// background is refused with `cannotInterruptOthers`), confirm the
        /// HFP route, then switch the ACTIVE CATEGORY to `.record` — and only
        /// then build and start the engine.
        ///
        /// September 10: background requests were rejected with !int; the
        /// getter then reported playAndRecord with empty options. Successful
        /// capture after that rejection is NOT successful record activation.
        /// It differs from `interruptAfterCapture`
        /// in both what changes (the category, not just the options) and when
        /// (before `AVAudioEngine.start()`, not after).
        ///
        /// The hypothesis — ours, not an Apple-recommended workaround — is
        /// that `.record`, whose purpose is recording while silencing system
        /// output, forces the other player off the headset BEFORE the engine
        /// start that currently fails. If `.record` instead drops the HFP
        /// input, is refused in the background, or leaves the engine unable to
        /// start, the arm dies and the trace says which.
        case recordBeforeEngine = "record_before_engine"

        /// **R2.** Same activation, but the engine starts first and is
        /// confirmed to be delivering PCM; only then does the category switch
        /// to `.record`. Answers a different question: can `.record` be our
        /// steady-state category once capture is already alive?
        ///
        /// If R2 works and R1 does not, `.record` is a post-start media
        /// policy and not a fix for cold engine contention.
        case recordAfterEngine = "record_after_engine"

        /// Whether this arm switches the category (rather than only options).
        var switchesToRecordCategory: Bool {
            self == .recordBeforeEngine || self == .recordAfterEngine
        }
    }

    /// **WARM.** How long the audio session, `AVAudioEngine` and input tap are
    /// held alive AFTER a dictation ends, so the next background start has no
    /// Bluetooth I/O to acquire at all.
    ///
    /// The measured failure is `AVAudioEngine.start()` refused from the
    /// background while another app holds the headset. If the engine is
    /// already running and the session already owns the HFP route, that call
    /// never happens — the failure point leaves the start path entirely. Two
    /// public implementations (KeyVox, VivaDicta) ship this shape, and
    /// VivaDicta's source says outright that it keeps the Bluetooth session
    /// alive while idle because ending it breaks background keyboard
    /// recording.
    ///
    /// Deliberately BOUNDED for dogfood: a hot microphone indefinitely is a
    /// product decision, not a diagnostic. Battery and the mic indicator are
    /// measured after ownership is settled, not before.
    ///
    /// DELETE, or promote to a real setting, once the question is answered.
    enum WarmHold: String {
        /// Shipping: stop the engine and deactivate the session on every stop.
        case off
        case seconds30 = "30"
        case seconds180 = "180"

        var idleSeconds: TimeInterval? {
            switch self {
            case .off: return nil
            case .seconds30: return 30
            case .seconds180: return 180
            }
        }
    }

    /// Phase 4 — one cheap session variable at a time, all under the exact
    /// `background + HFP + other audio playing` condition. NONE of these is a
    /// regression delta: the known-good build did all the same things. They
    /// are current-iOS robustness experiments, ranked below WARM.
    ///
    /// DELETE with the rest of the diagnostic scaffolding.
    enum SessionTweak: String {
        /// Shipping behaviour.
        case none

        /// **S1.** Do not ask for a 16 kHz session rate; let the hardware
        /// negotiate its native format and convert downstream, which is what
        /// KeyVox and VivaDicta both do. KeyVox's source says outright that an
        /// HFP route can reject a forced 16 kHz preference. Our conversion to
        /// mono 16 kHz is unchanged either way — only the ASK is dropped.
        case noPreferredRate = "no_preferred_rate"
        // Explicit rate comparison; skipping a setter can retain the prior preference.
        case preferredRate48k = "preferred_rate_48k"

        /// **S2.** Offer Core Audio both Bluetooth route families at once.
        /// Apple permits both options together and gives HFP priority when one
        /// device supports both; Telegram sets both for some configurations.
        /// This is NOT expected to give simultaneous A2DP output and HFP mic
        /// on one headset — the question is only whether offering both changes
        /// the handoff.
        case allowA2DP = "allow_a2dp"

        /// **S3.** Let iOS choose the input instead of pinning the Bluetooth
        /// port, which is what both public implementations' automatic paths
        /// do. The route is still VERIFIED afterwards — the product invariant
        /// that a headset must never silently become the phone mic is not
        /// weakened, only the unconditional pin is.
        case systemRouting = "system_routing"

        /// **S4.** Drop the two newer session preferences
        /// (`setPrefersNoInterruptionsFromSystemAlerts`, haptics/system sounds
        /// during recording). Lowest probability of the four: both public
        /// implementations use the haptics preference successfully. The
        /// no-system-alerts preference is the one absent from the old
        /// known-good session, and it is cheap to isolate.
        case noSessionPreferences = "no_session_preferences"

        /// Built-in route isolation: do not advertise Bluetooth input when
        /// no Bluetooth device participates in this test.
        case noBluetoothOption = "no_bluetooth_option"

        /// Built-in route isolation: do not force receiver output to speaker.
        case noDefaultToSpeaker = "no_default_to_speaker"

        /// Request phone input + receiver even when Bluetooth playback is connected.
        case receiverAndPhoneMic = "receiver_and_phone_mic"

        /// Require headset capture; never take the background phone-mic fallback.
        case bluetoothMicNoSpeaker = "bluetooth_mic_no_speaker"

        /// Extend 15E: use an available headset mic, otherwise keep 15E exactly.
        case bluetoothMicOrReceiver = "bluetooth_mic_or_receiver"

        /// Use August 30's system-selected HFP profile when Bluetooth input is
        /// available; otherwise use tested 15E phone-mic + receiver routing.
        case historicalBluetoothOrReceiver = "historical_bluetooth_or_receiver"

        /// Built-in route isolation: request only background mixing.
        case minimalBuiltIn = "minimal_built_in"

        /// Keep existing session policy, but disable RemoteIO's playback bus
        /// before starting AVAudioEngine. AudioRecordingIntent grants a
        /// background recording assertion; it does not grant mixable playback.
        case inputOnlyRemoteIO = "input_only_remote_io"
    }

    /// Set when we deliberately re-`setCategory` a live session, so the
    /// interruption our own switch provokes is not mistaken for an external
    /// one. `nil` whenever no switch is in flight.
    static var selfInterruptSuppressedUntil: Date?

    /// Which Bluetooth microphone path ended up active for a recording,
    /// classified from the activated session's route.
    enum BluetoothTransport: String {
        case none
        case hfp
        case highQuality = "high_quality"
        case bluetoothLE = "bluetooth_le"
    }

    /// Thrown when a Bluetooth input was present before activation but the
    /// route that actually activated is not that Bluetooth input (e.g. iOS
    /// silently fell back to the built-in mic). Treated as a retryable,
    /// session-level failure by the route-recovery ladder in
    /// `RecordingAudioSession.swift` so the next attempt rebuilds the
    /// session rather than quietly recording from the wrong microphone.
    enum PolicyError: Error {
        case bluetoothInputNotSelected
        case phoneMicrophoneNotSelected
        /// `activate(options:)` resolved with `activated == false` and no
        /// thrown error — the call completed but the session is not active,
        /// so engine setup must not run. Session-level and retryable like
        /// the route-mismatch cases above: the next attempt rebuilds the
        /// session rather than retrying in place.
        case sessionNotActivated
    }

    /// Retained single-case knob for the session CATEGORY, so the
    /// `category_policy` telemetry field and the dogfood deep-link vocabulary
    /// stay stable across builds.
    ///
    /// The `.record` arm is retired: it forbids `.defaultToSpeaker`, and the real fix
    /// for background `cannotInterruptOthers` is unconditional mixability on
    /// background-originated starts, not a different category. A device still
    /// pinned to the retired `record` raw value decodes to `nil` and falls
    /// back to `.playAndRecord` automatically.
    enum CategoryPolicy: String {
        case playAndRecord = "play_and_record"
        case record
        case multiRoute = "multi_route"
    }

    private static let mixPolicyDefaultsKey = "heardright.dogfood.audioMixPolicy.v1"
    private static let categoryPolicyDefaultsKey = "heardright.dogfood.audioCategoryPolicy.v1"
    private static let sessionProfileDefaultsKey = "heardright.dogfood.audioSessionProfile.v1"
    private static let retryLadderDefaultsKey = "heardright.dogfood.audioRetryLadder.v1"
    private static let engineRecoveryDefaultsKey = "heardright.dogfood.audioEngineRecovery.v1"
    private static let mixSwitchDefaultsKey = "heardright.dogfood.audioMixSwitch.v1"
    private static let warmHoldDefaultsKey = "heardright.dogfood.audioWarmHold.v1"
    private static let sessionTweakDefaultsKey = "heardright.dogfood.audioSessionTweak.v1"
    private static let foregroundOnRefusalDefaultsKey = "heardright.dogfood.audioForegroundOnRefusal.v1"

    private static var defaults: UserDefaults {
        UserDefaults(suiteName: HeardRightShared.appGroup) ?? .standard
    }

    /// Scope & settings are frozen once per capture. Foreground uses the
    /// committed pre-experiment reference, never persisted dogfood knobs.
    struct CaptureConfiguration: Equatable {
        let startedInBackground: Bool
        let usesReferenceSettings: Bool
        let mixPolicy: MixPolicy
        let categoryPolicy: CategoryPolicy
        let profile: SessionProfile
        let tweak: SessionTweak
        let ladder: RetryLadder
        let recovery: EngineRecovery
        let mixSwitch: MixSwitch
        let voiceProcessing: Bool
        let alternativeCaptureProbe: Bool
        let prefersFarFieldInput: Bool
        let usesDualRoute: Bool
        /// Minimum time between confirmed HFP route & first engine start.
        /// Dogfood-only: isolates route-transition latency from every other
        /// session, capture & retry setting.
        let bluetoothSettleMs: UInt64
        var testPresetID: Int? = nil

        static let foreground = CaptureConfiguration(
            startedInBackground: false, usesReferenceSettings: true,
            mixPolicy: .mixWithOthers, categoryPolicy: .playAndRecord,
            profile: .legacyBose, tweak: .noSessionPreferences,
            ladder: .standard, recovery: .sameSession, mixSwitch: .none,
            voiceProcessing: false, alternativeCaptureProbe: false,
            prefersFarFieldInput: false, usesDualRoute: false,
            bluetoothSettleMs: 0
        )

        /// Device-tested 15E production policy: acquire background recording
        /// mixably, move playback to receiver, capture built-in mic, then
        /// remove mixing before starting engine.
        static let background15E = CaptureConfiguration(
            startedInBackground: true, usesReferenceSettings: false,
            mixPolicy: .backgroundDuck, categoryPolicy: .playAndRecord,
            profile: .legacyDefaultMode, tweak: .receiverAndPhoneMic,
            ladder: .standard, recovery: .sameSession,
            mixSwitch: .nonmixBeforeEngine, voiceProcessing: false,
            alternativeCaptureProbe: false, prefersFarFieldInput: false,
            usesDualRoute: false, bluetoothSettleMs: 0
        )
    }

    static func captureConfiguration(appActive: Bool) -> CaptureConfiguration {
        // Foreground stays frozen. Release builds always use device-tested
        // 15E & never read persisted dogfood choices.
        if appActive { return .foreground }
        #if DEBUG
        if let preset = selectedTestPreset { return preset.configuration }
        #endif
        return .background15E
    }


    // Stable, numbered presets. Append new cases; never renumber a tested case.
    struct TestPreset: Identifiable {
        let id: Int
        let group: String
        let title: String
        let detail: String
        let configuration: CaptureConfiguration
        var displayNumber: String {
            switch id {
            case 65: return "23B"
            case 66: return "15B (original)"
            case 67: return "15C"
            case 68: return "15D"
            case 69: return "15B"
            case 70: return "15E"
            case 71: return "BT1 (original)"
            case 72: return "BT2 (original)"
            case 73: return "BT1"
            case 74: return "BT2"
            case 75: return "BT2 · 250"
            case 76: return "BT2 · 750"
            case 77: return "BT2 · 1500"
            case 78: return "BT2 · Long"
            case 79: return "BT2 · Spoken"
            case 80: return "BT2 · Spoken 750"
            case 81: return "Aug 30"
            case 82: return "Aug 30 · Current retry"
            case 83: return "Aug 30 · Route event"
            case 84: return "Aug 30 · Long retry"
            case 85: return "iOS 27 · Far field"
            case 86: return "iOS 27 · Dual route"
            default: return String(id)
            }
        }
    }

    static let testPresetDefaultsKey = "heardright.dogfood.testPreset.v1"
    static let testPresets: [TestPreset] = {
        var presets: [TestPreset] = []
        func add(_ title: String, _ group: String, profile: SessionProfile = .legacyDefaultMode,
                 mix: MixPolicy = .mixWithOthers, transition: MixSwitch = .none,
                 tweak: SessionTweak = .none, recovery: EngineRecovery = .sameSession,
                 ladder: RetryLadder = .standard, category: CategoryPolicy = .playAndRecord,
                 bluetoothSettleMs: UInt64 = 0, usesReferenceSettings: Bool = false,
                 farField: Bool = false, dualRoute: Bool = false) {
            let id = presets.count + 1
            let configuration = CaptureConfiguration(
                startedInBackground: true, usesReferenceSettings: usesReferenceSettings,
                mixPolicy: mix, categoryPolicy: category, profile: profile, tweak: tweak,
                ladder: ladder, recovery: recovery, mixSwitch: transition,
                voiceProcessing: false, alternativeCaptureProbe: false,
                prefersFarFieldInput: farField, usesDualRoute: dualRoute,
                bluetoothSettleMs: bluetoothSettleMs, testPresetID: id)
            let detail = [category.rawValue, profile.mode.rawValue, mix.rawValue,
                          transition == .none ? "no category switch" : transition.rawValue,
                          tweak.rawValue, recovery.rawValue, ladder.rawValue,
                          farField ? "far-field input" : "standard input",
                          dualRoute ? "dual route" : "single route",
                          bluetoothSettleMs == 0 ? "no HFP wait" : "\(bluetoothSettleMs) ms HFP wait"]
                .joined(separator: " · ").replacingOccurrences(of: "_", with: " ")
            presets.append(TestPreset(id: id, group: group, title: title,
                                      detail: detail, configuration: configuration))
        }
        let modes: [(SessionProfile, String)] = [(.legacyDefaultMode, "Default"), (.legacyBose, "Spoken audio")]
        let mixes: [(MixPolicy, String)] = [(.nonMixing, "No mixing"), (.mixWithOthers, "Mix"),
            (.backgroundDuck, "Duck music"), (.backgroundInterruptSpoken, "Interrupt spoken audio"),
            (.backgroundDuckAndSpoken, "Duck + interrupt spoken audio")]
        // First ten isolate mode & mixing without changing a live session.
        for (profile, mode) in modes {
            for (mix, name) in mixes {
                add("\(mode) · \(name)", "Start here", profile: profile, mix: mix)
            }
        }
        let transitions: [(MixSwitch, String)] = [(.nonmixBeforeEngine, "Remove mixing before engine"),
            (.interruptAfterCapture, "Remove mixing after capture"),
            (.recordBeforeEngine, "Switch to record before engine"),
            (.recordAfterEngine, "Switch to record after engine")]
        for (profile, mode) in modes {
            for (mix, name) in mixes where mix != .nonMixing {
                for (transition, label) in transitions {
                    add("\(mode) · \(name) · \(label)", "Category transitions",
                        profile: profile, mix: mix, transition: transition)
                }
            }
        }
        // Direct .record is intentionally NON-mixable; no invalid record+mix option.
        add("Record category from start", "Input preferences", mix: .nonMixing, category: .record)
        let tweaks: [(SessionTweak, String)] = [(.noPreferredRate, "Native sample rate"),
            (.noSessionPreferences, "No alert or haptic preferences"),
            (.noBluetoothOption, "No Bluetooth option"), (.noDefaultToSpeaker, "No speaker override"),
            (.minimalBuiltIn, "Minimal session options"), (.inputOnlyRemoteIO, "Input-only engine"),
            (.allowA2DP, "Allow A2DP output")]
        for (mix, name) in [(MixPolicy.mixWithOthers, "Mix"), (.backgroundDuck, "Duck")] {
            for (tweak, label) in tweaks {
                add("\(name) · \(label)", "Input preferences", mix: mix, tweak: tweak)
            }
            for (recovery, label) in [(EngineRecovery.rebuildSession, "Rebuild session on retry"),
                (.awaitRouteChange, "Wait for route on retry"), (.rebuildAndAwaitRoute, "Wait + rebuild on retry")] {
                add("\(name) · \(label)", "Recovery", mix: mix, recovery: recovery)
            }
        }
        add("Mix · Extended retry window", "Recovery", ladder: .long)
        // Append only: keep the existing 64 option IDs/configurations unchanged.
        add("Default · Keep mixing · Remove ducking before engine", "Category transitions",
            mix: .backgroundDuckAndSpoken, transition: .unduckBeforeEngine)
        // One-variable variants of 15. Keep all prior IDs unchanged.
        add("Keep mixing · Remove only ducking", "Category transitions",
            mix: .backgroundDuck, transition: .unduckBeforeEngine)
        add("Option 15 · Request 48 kHz", "Category transitions",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine, tweak: .preferredRate48k)
        add("Option 15 · No speaker override", "Category transitions",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine, tweak: .noDefaultToSpeaker)
        // New IDs preserve the earlier 15B results and leave 15D unchanged.
        add("Keep mixing · No speaker override", "Category transitions",
            mix: .backgroundDuck, transition: .unduckBeforeEngine, tweak: .noDefaultToSpeaker)
        add("Request earpiece + phone mic", "Category transitions",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine, tweak: .receiverAndPhoneMic)
        add("Bluetooth mic · Remove mixing before engine", "Category transitions",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine, tweak: .bluetoothMicNoSpeaker)
        add("Bluetooth mic · Keep ducking + mixing", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicNoSpeaker)
        add("15E + Bluetooth mic · Remove mixing", "Category transitions",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine, tweak: .bluetoothMicOrReceiver)
        add("15E + Bluetooth mic · Keep ducking + mixing", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver)
        add("BT2 · Wait 250 ms after HFP", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver, bluetoothSettleMs: 250)
        add("BT2 · Wait 750 ms after HFP", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver, bluetoothSettleMs: 750)
        add("BT2 · Wait 1500 ms after HFP", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver, bluetoothSettleMs: 1_500)
        add("BT2 · Extended engine retry", "Category transitions",
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver, ladder: .long)
        add("BT2 · Spoken-audio mode", "Category transitions", profile: .legacyBose,
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver)
        add("BT2 · Spoken-audio + 750 ms", "Category transitions", profile: .legacyBose,
            mix: .backgroundDuck, tweak: .bluetoothMicOrReceiver, bluetoothSettleMs: 750)
        // Exact pre-August-31 Bluetooth routing: spokenAudio, HFP, speaker,
        // mixWithOthers, 16 kHz mono, system-selected input, no route rejection.
        add("Exact historical profile", "Historical Bluetooth", profile: .legacyBose,
            mix: .mixWithOthers, tweak: .historicalBluetoothOrReceiver, ladder: .august30,
            usesReferenceSettings: true)
        add("Historical profile + current retry", "Historical Bluetooth", profile: .legacyBose,
            mix: .mixWithOthers, tweak: .historicalBluetoothOrReceiver,
            usesReferenceSettings: true)
        add("Historical profile + route-event retry", "Historical Bluetooth", profile: .legacyBose,
            mix: .mixWithOthers, tweak: .historicalBluetoothOrReceiver, recovery: .awaitRouteChange,
            ladder: .august30, usesReferenceSettings: true)
        add("Historical profile + extended retry", "Historical Bluetooth", profile: .legacyBose,
            mix: .mixWithOthers, tweak: .historicalBluetoothOrReceiver, ladder: .long,
            usesReferenceSettings: true)
        add("15E Bluetooth + far-field input", "iOS 27 Bluetooth",
            mix: .backgroundDuck, transition: .nonmixBeforeEngine,
            tweak: .bluetoothMicOrReceiver, farField: true)
        add("Multi-route + dual-route", "iOS 27 Bluetooth",
            mix: .mixWithOthers, tweak: .bluetoothMicOrReceiver,
            category: .multiRoute, dualRoute: true)
        return presets
    }()

    static var selectedTestPreset: TestPreset? {
        let id = defaults.integer(forKey: testPresetDefaultsKey)
        return testPresets.first { $0.id == id }
    }

    static func selectTestPreset(_ id: Int?) {
        guard id == nil || testPresets.contains(where: { $0.id == id }) else { return }
        defaults.set(id ?? 0, forKey: testPresetDefaultsKey)
        DogfoodTraceStore.record("audio_test_preset_selected", fields: [
            "preset_id": id ?? 0, "preset_title": selectedTestPreset?.title ?? "Previous settings",
            "scope": "background_only", "warm_hold": "off"
        ])
    }

    static func testResult(preset: Int, scenario: String) -> String {
        defaults.string(forKey: "heardright.audioTestResult.v1.\(preset).\(scenario)") ?? "Not tested"
    }

    static func setTestResult(_ result: String, preset: Int, scenario: String) {
        defaults.set(result, forKey: "heardright.audioTestResult.v1.\(preset).\(scenario)")
        DogfoodTraceStore.record("audio_test_user_result", fields: [
            "preset_id": preset, "scenario": scenario, "result": result
        ])
    }

    static func currentMixPolicy() -> MixPolicy {
        if usesAugust30AudioSettings { return .mixWithOthers }
        guard let raw = defaults.string(forKey: mixPolicyDefaultsKey),
              let policy = MixPolicy(rawValue: raw) else {
            return .backgroundMixOnly
        }
        return policy
    }

    static func setMixPolicy(_ policy: MixPolicy) {
        defaults.set(policy.rawValue, forKey: mixPolicyDefaultsKey)
    }

    static func currentCategoryPolicy() -> CategoryPolicy {
        guard let raw = defaults.string(forKey: categoryPolicyDefaultsKey),
              let policy = CategoryPolicy(rawValue: raw) else {
            return .playAndRecord
        }
        return policy
    }

    static func setCategoryPolicy(_ policy: CategoryPolicy) {
        defaults.set(policy.rawValue, forKey: categoryPolicyDefaultsKey)
    }

    /// Defaults to `.current` so an un-pinned device keeps shipping behavior;
    /// an unknown/retired raw value falls back the same way.
    static func currentSessionProfile() -> SessionProfile {
        if usesAugust30AudioSettings { return .legacyBose }
        guard let raw = defaults.string(forKey: sessionProfileDefaultsKey),
              let profile = SessionProfile(rawValue: raw) else {
            return .current
        }
        return profile
    }

    static func setSessionProfile(_ profile: SessionProfile) {
        defaults.set(profile.rawValue, forKey: sessionProfileDefaultsKey)
    }

    static func currentRetryLadder() -> RetryLadder {
        guard let raw = defaults.string(forKey: retryLadderDefaultsKey),
              let ladder = RetryLadder(rawValue: raw) else {
            return .standard
        }
        return ladder
    }

    static func setRetryLadder(_ ladder: RetryLadder) {
        defaults.set(ladder.rawValue, forKey: retryLadderDefaultsKey)
    }

    static func currentEngineRecovery() -> EngineRecovery {
        guard let raw = defaults.string(forKey: engineRecoveryDefaultsKey),
              let value = EngineRecovery(rawValue: raw) else {
            return .sameSession
        }
        return value
    }

    static func setEngineRecovery(_ value: EngineRecovery) {
        defaults.set(value.rawValue, forKey: engineRecoveryDefaultsKey)
    }

    static func currentMixSwitch() -> MixSwitch {
        if usesAugust30AudioSettings { return .none }
        guard let raw = defaults.string(forKey: mixSwitchDefaultsKey),
              let value = MixSwitch(rawValue: raw) else {
            return .none
        }
        return value
    }

    static func setMixSwitch(_ value: MixSwitch) {
        defaults.set(value.rawValue, forKey: mixSwitchDefaultsKey)
    }

    static func currentWarmHold() -> WarmHold {
        // Retired experiment: persisted dogfood values must never keep the
        // microphone active after the user presses Stop or Cancel.
        return .off
    }

    static func setWarmHold(_ value: WarmHold) {
        defaults.removeObject(forKey: warmHoldDefaultsKey)
    }

    static func currentSessionTweak() -> SessionTweak {
        if usesAugust30AudioSettings { return .noSessionPreferences }
        guard let raw = defaults.string(forKey: sessionTweakDefaultsKey),
              let value = SessionTweak(rawValue: raw) else {
            return .none
        }
        return value
    }

    static func setSessionTweak(_ value: SessionTweak) {
        defaults.set(value.rawValue, forKey: sessionTweakDefaultsKey)
    }


    /// Phase 6 dogfood arm: when a BACKGROUND intent start exhausts the
    /// engine ladder with a Core Audio refusal, ask iOS to continue the intent
    /// in the foreground and start once more there. Foreground on the same
    /// route and with the same competing playback is device-proven to
    /// deliver healthy audio (peak 0.2047) where the
    /// backgrounded attempt seconds earlier was silent. The intents already
    /// declare `.foreground(.dynamic)`; this only decides whether a refusal
    /// uses it. Off by default: shipping behaviour is unchanged.
    static func foregroundOnRefusalEnabled() -> Bool {
        defaults.bool(forKey: foregroundOnRefusalDefaultsKey)
    }

    private static let foregroundForMediaDefaultsKey = "heardright.dogfood.audioForegroundForMedia.v1"

    /// Background-originated start with other audio playing: continue the
    /// intent into the foreground BEFORE the session is composed, so the
    /// non-mixing foreground policy interrupts that audio. Device-proven
    /// REFUSED by iOS 26 from an Action Button start, unlocked or locked
    /// (`AppIntentError` "This current action is not allowed", 1 ms; see
    /// docs/results/2026-09-06-ios-foreground-for-media.md). OFF by default:
    /// the proven mixable background path runs and media keeps playing. The
    /// arm stays for re-testing on a future iOS.
    static func foregroundForMediaEnabled() -> Bool {
        if usesAugust30AudioSettings { return false }
        return defaults.bool(forKey: foregroundForMediaDefaultsKey)
    }

    private static let appleMusicControlDefaultsKey = "heardright.dogfood.appleMusicControl.v1"

    /// Background starts pause the system Music player through
    /// `MPMusicPlayerController` with session ownership (see
    /// `AppleMusicPlaybackOwner`). ON by default in this dogfood build for
    /// the physical test; `heardright://dogfood-audio-music?control=off`.
    static func appleMusicControlEnabled() -> Bool {
        if usesAugust30AudioSettings { return false }
        if defaults.object(forKey: appleMusicControlDefaultsKey) == nil { return true }
        return defaults.bool(forKey: appleMusicControlDefaultsKey)
    }

    static func setAppleMusicControlEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: appleMusicControlDefaultsKey)
    }

    static func setForegroundForMediaEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: foregroundForMediaDefaultsKey)
    }

    static func setForegroundOnRefusalEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: foregroundOnRefusalDefaultsKey)
    }

    /// Does this start failure look like Core Audio refusing microphone I/O to
    /// a backgrounded app, rather than a permission, model, ownership or
    /// Live Activity problem? Matches the measured signatures only:
    /// `kAUStartIO` (avfaudio 2003329396) from `AVAudioEngine.start()`, a
    /// dead capture path after a nominal start, and a session activation
    /// refused as unable to interrupt others (560557684) or contended
    /// (561015905).
    static func isBackgroundStartRefusal(_ error: Error) -> Bool {
        if case WarmAudioCapture.CaptureError.capturePathDead = error { return true }
        if error is WarmAudioCapture.CaptureError { return false }
        if error is PolicyError { return false }
        let nsError = error as NSError
        if nsError.domain == "com.apple.coreaudio.avfaudio" { return true }
        return nsError.code == 2003329396
            || nsError.code == 560557684
            || nsError.code == AudioCaptureRecoveryPolicy.sessionActivationContendedCode
    }

    /// The `AVAudioSession.Category` for a `CategoryPolicy`.
    static func avCategory(for categoryPolicy: CategoryPolicy) -> AVAudioSession.Category {
        switch categoryPolicy {
        case .playAndRecord:
            return .playAndRecord
        case .record:
            return .record
        case .multiRoute:
            return .multiRoute
        }
    }

    /// Base session options, before `startSession` composes in the iOS 26
    /// `.bluetoothHighQualityRecording` option. The category is always
    /// `.playAndRecord`, so `.defaultToSpeaker` is always valid.
    ///
    /// Background ducking experiment is approved. Foreground
    /// preserves the August 30 reference; spoken-audio interruption stays off.
    static func baseOptions(
        for mixPolicy: MixPolicy,
        appActive: Bool,
        profile: SessionProfile = .current,
        tweak: SessionTweak = .none,
        explicitMixPolicy: Bool = false
    ) -> AVAudioSession.CategoryOptions {
        var options: AVAudioSession.CategoryOptions = []
        if tweak != .noDefaultToSpeaker && tweak != .minimalBuiltIn && tweak != .receiverAndPhoneMic && tweak != .bluetoothMicNoSpeaker && tweak != .bluetoothMicOrReceiver {
            options.insert(.defaultToSpeaker)
        }
        if tweak != .noBluetoothOption && tweak != .minimalBuiltIn && tweak != .receiverAndPhoneMic {
            options.insert(.allowBluetoothHFP)
        }
        if tweak == .allowA2DP {
            options.insert(.allowBluetoothA2DP)
        }
        // Foreground must interrupt media; legacy always-mix applies only in background.
        if appActive { return options }
        // Explicit media experiments compose with the selected legacy routing
        // profile. Otherwise its early return would silently mask this knob.
        switch mixPolicy {
        case .backgroundDuck, .backgroundDuckAndSpoken:
            options.formUnion([.mixWithOthers, .duckOthers])
            if mixPolicy == .backgroundDuckAndSpoken {
                options.insert(.interruptSpokenAudioAndMixWithOthers)
            }
            return options
        case .backgroundInterruptSpoken:
            options.formUnion([.mixWithOthers, .interruptSpokenAudioAndMixWithOthers])
            return options
        default:
            break
        }
        // A legacy A/B arm reproduces its own shape and ignores the mix knob.
        if profile.forcesMixWithOthers && !explicitMixPolicy {
            options.insert(.mixWithOthers)
            return options
        }
        switch mixPolicy {
        case .mixWithOthers:
            options.insert(.mixWithOthers)
        case .backgroundMixOnly:
            // Background-originated starts are UNCONDITIONALLY mixable.
            // A non-mixable `.playAndRecord` session activated while
            // backgrounded is rejected with `cannotInterruptOthers`
            // (560557684) whether or not anything is actually playing, so
            // mixability is what makes an Action Button / Shortcut / Control
            // Center start work at all. A foreground start stays non-mixable
            // (other audio stops), which is the preferred in-app behavior.
            if !appActive {
                options.insert(.mixWithOthers)
            }
        case .backgroundInterruptSpoken:
            if !appActive {
                options.insert(.interruptSpokenAudioAndMixWithOthers)
            }
        case .backgroundDuck, .backgroundDuckAndSpoken:
            break // Composed above, before legacy-profile overrides.
        case .nonMixing:
            break
        }
        return options
    }

    /// Read `UIApplication.shared.applicationState` on the main actor. Falls
    /// back to `true` (foreground) when UIKit is unavailable, matching
    /// callers that have no UI context.
    static func isAppActive() -> Bool {
        #if canImport(UIKit)
        return UIApplication.shared.applicationState == .active
        #else
        return true
        #endif
    }

    /// Sorted option names for telemetry — never the raw `OptionSet` value,
    /// which is content-free but not directly JSON-serializable.
    static func optionNames(_ options: AVAudioSession.CategoryOptions) -> [String] {
        var names: [String] = []
        if options.contains(.defaultToSpeaker) { names.append("defaultToSpeaker") }
        if options.contains(.allowBluetoothHFP) { names.append("allowBluetoothHFP") }
        if options.contains(.allowBluetoothA2DP) { names.append("allowBluetoothA2DP") }
        if options.contains(.mixWithOthers) { names.append("mixWithOthers") }
        if options.contains(.duckOthers) { names.append("duckOthers") }
        if options.contains(.interruptSpokenAudioAndMixWithOthers) {
            names.append("interruptSpokenAudioAndMixWithOthers")
        }
        if #available(iOS 26.0, *) {
            if options.contains(.bluetoothHighQualityRecording) {
                names.append("bluetoothHighQualityRecording")
            }
        }
        return names.sorted()
    }

    /// One session-start attempt's plan, snapshotted for telemetry. Every
    /// field is content-free: port TYPES only, never `portName`/`UID`.
    struct Plan {
        let profile: SessionProfile
        /// The mode actually passed to `setCategory` for this attempt.
        let mode: AVAudioSession.Mode
        let mixPolicy: MixPolicy
        let categoryPolicy: CategoryPolicy
        let baseOptions: AVAudioSession.CategoryOptions
        /// Sorted final option names actually used for `setCategory`,
        /// e.g. ["allowBluetoothHFP", "bluetoothHighQualityRecording", "defaultToSpeaker"].
        let options: [String]
        let bluetoothInputPresent: Bool
        let bluetoothInputPortTypes: [String]
        let hqAPIAvailable: Bool
        let hqSupported: Bool?
        let hqRequested: Bool
        /// Whether `setPreferredInput` was called for a Bluetooth port. `var`
        /// because Apple requires that call to happen only AFTER the session
        /// is activated, so it is filled in once activation succeeds.
        var preferredInputSet: Bool
        /// Whether `UIApplication.shared.applicationState == .active` at the
        /// top of `startSession`, snapshotted once so the A/B policies and
        /// telemetry agree on foreground/background classification.
        let appActive: Bool
    }

    /// What actually happened after `setActive` was attempted for a `Plan`.
    struct Outcome {
        let activated: Bool
        let routeInputs: [String]
        let routeOutputs: [String]
        let hqEnabled: Bool?
        let bluetoothTransport: BluetoothTransport
        let mixWithOthersActive: Bool
        /// `session.categoryOptions.contains(.duckOthers)` post-activation.
        let duckOthersActive: Bool
        let errorDomain: String?
        let errorCode: Int?
    }

    /// The most recent start attempt's plan/outcome, so a failure path far
    /// from `startSession` (e.g. `RecordingSystemSurface`'s catch) can
    /// still attach full policy context to its own trace event.
    /// When the Bluetooth route last became ready, so the engine-start
    /// telemetry can report the route-ready → `AVAudioEngine.start()`
    /// interval. Stored here because `SharedRecordingCoordinator`'s audio
    /// session code lives in an extension and cannot add stored properties.
    static var lastRouteReadyAt: DispatchTime?

    static var lastAttempt: Plan?
    static var lastOutcome: Outcome?

    /// Classify the active Bluetooth transport from the current route's
    /// first input port and (iOS 26+) whether high-quality recording is
    /// enabled on it.
    static func classifyTransport(portType: AVAudioSession.Port?, hqEnabled: Bool?) -> BluetoothTransport {
        guard let portType else { return .none }
        switch portType {
        case .bluetoothHFP:
            return hqEnabled == true ? .highQuality : .hfp
        case .bluetoothLE:
            return .bluetoothLE
        default:
            return .none
        }
    }

    static func telemetryFields(for plan: Plan, outcome: Outcome?) -> [String: Any] {
        var fields: [String: Any] = [
            "mix_policy": plan.mixPolicy.rawValue,
            "category_policy": plan.categoryPolicy.rawValue,
            "audio_options": plan.options.joined(separator: ","),
            "audio_category": RecordingAudioPolicy.avCategory(for: plan.categoryPolicy).rawValue,
            "audio_mode": plan.mode.rawValue,
            "session_profile": plan.profile.rawValue,
            "bluetooth_input_present": plan.bluetoothInputPresent,
            "bluetooth_input_port_types": plan.bluetoothInputPortTypes.joined(separator: ","),
            "hq_api_available": plan.hqAPIAvailable,
            "hq_supported": plan.hqSupported ?? "unknown",
            "hq_requested": plan.hqRequested,
            "preferred_input_set": plan.preferredInputSet,
            "app_active": plan.appActive,
        ]
        guard let outcome else { return fields }
        fields["session_activated"] = outcome.activated
        fields["route_inputs"] = outcome.routeInputs.joined(separator: ",")
        fields["route_outputs"] = outcome.routeOutputs.joined(separator: ",")
        fields["hq_enabled"] = outcome.hqEnabled ?? "unknown"
        fields["bluetooth_transport"] = outcome.bluetoothTransport.rawValue
        fields["mix_with_others_active"] = outcome.mixWithOthersActive
        fields["duck_others_active"] = outcome.duckOthersActive
        if let domain = outcome.errorDomain { fields["error_domain"] = domain }
        if let code = outcome.errorCode { fields["error_code"] = code }
        return fields
    }
}
#endif
