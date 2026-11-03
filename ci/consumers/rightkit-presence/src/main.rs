// rightkit-presence 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Classifies synthetic window observations and layers preferences. The polling loop is never
// started and no Do-Not-Disturb or window state is changed; the one live call is a read-only probe
// whose answer is not constrained (a headless host may refuse it).
use rightkit_presence::presence::{
    controls_supported, detect, observe_os, reconcile, AutoPrefs, AutoState, Environment,
    PresenceDnd, Rect, WindowObservation, CONTROLS_SUPPORTED, OBSERVATION_HOLD, POLL_SECONDS,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn window(owner: &str, title: &str, layer: i32, bounds: Option<Rect>) -> WindowObservation {
    WindowObservation {
        owner: owner.to_string(),
        title: title.to_string(),
        layer,
        bounds,
    }
}

fn main() {
    check(
        "poll interval",
        POLL_SECONDS == 2 && OBSERVATION_HOLD.as_secs() == 30,
    );
    check(
        "controls_supported mirrors the constant",
        controls_supported() == CONTROLS_SUPPORTED,
    );

    // Detection over synthetic observations on a 1920x1080 monitor.
    let monitor = Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };
    let full = Some(monitor);
    let small = Some(Rect {
        x: 100.0,
        y: 100.0,
        width: 300.0,
        height: 200.0,
    });

    let quiet = detect(monitor, &[window("Finder", "Documents", 0, small)], "MyApp");
    check(
        "small ordinary window is quiet",
        quiet == Environment::default(),
    );

    let call = detect(
        monitor,
        &[window("zoom.us", "Zoom Meeting", 0, full)],
        "MyApp",
    );
    check(
        "full-screen meeting is both",
        call.fullscreen_on_main_monitor && call.meeting_or_share_active,
    );

    let side = detect(
        monitor,
        &[window(
            "Slack",
            "Huddle",
            0,
            Some(Rect {
                x: 0.0,
                y: 0.0,
                width: 960.0,
                height: 600.0,
            }),
        )],
        "MyApp",
    );
    check(
        "large meeting window is not fullscreen",
        !side.fullscreen_on_main_monitor && side.meeting_or_share_active,
    );

    let ignored = detect(
        monitor,
        &[
            window("Dock", "", 0, full),
            window("MyApp", "Zoom Meeting", 0, full),
            window("Zoom", "Zoom Meeting", 25, full),
            window("Zoom", "Zoom Meeting", 0, None),
        ],
        "MyApp",
    );
    check(
        "dock, own, non-zero layer and boundless windows are ignored",
        ignored == Environment::default(),
    );

    // Preference gates clear automatic state when a control is off.
    let env = Environment {
        fullscreen_on_main_monitor: true,
        meeting_or_share_active: true,
    };
    let gated = reconcile(
        AutoPrefs {
            auto_hide_fullscreen: true,
            auto_dnd_meetings: false,
        },
        env,
    );
    check(
        "reconcile applies the gates",
        gated
            == AutoState {
                hidden_for_fullscreen: true,
                dnd_for_meeting: false,
            },
    );
    check(
        "default prefs clear everything",
        reconcile(AutoPrefs::default(), env) == AutoState::default(),
    );

    // Layering of manual and automatic Do-Not-Disturb.
    let state = PresenceDnd::new();
    check("starts quiet", !state.effective_dnd());
    state.set_prefs(AutoPrefs {
        auto_hide_fullscreen: true,
        auto_dnd_meetings: true,
    });
    check(
        "prefs round trip",
        state.prefs()
            == AutoPrefs {
                auto_hide_fullscreen: true,
                auto_dnd_meetings: true,
            },
    );
    check(
        "set_auto reports what changed",
        state.set_auto(AutoState {
            hidden_for_fullscreen: false,
            dnd_for_meeting: true,
        }) == (false, true),
    );
    check("auto dnd is effective", state.effective_dnd());
    check(
        "unchanged auto reports nothing",
        state.set_auto(AutoState {
            hidden_for_fullscreen: false,
            dnd_for_meeting: true,
        }) == (false, false),
    );
    state.set_auto(AutoState::default());
    state.set_manual_dnd(true);
    let snapshot = state.snapshot();
    check(
        "manual dnd is effective",
        snapshot.manual_dnd && !snapshot.auto_dnd && snapshot.effective_dnd,
    );
    check(
        "snapshot controls flag",
        snapshot.controls_supported == CONTROLS_SUPPORTED,
    );
    let wire = serde_json::to_value(snapshot).unwrap_or_default();
    check(
        "camelCase wire",
        wire["effectiveDnd"] == true && wire["manualDnd"] == true,
    );

    // An injected observation is held against live samples.
    let held = PresenceDnd::new();
    held.apply_observation(AutoState {
        hidden_for_fullscreen: true,
        dnd_for_meeting: false,
    });
    check(
        "live sample waits for the hold",
        held.apply_live(AutoState::default()).is_none() && held.snapshot().auto_hidden,
    );
    let live = PresenceDnd::new();
    check(
        "live sample applies without a hold",
        live.apply_live(AutoState {
            hidden_for_fullscreen: true,
            dnd_for_meeting: false,
        }) == Some((true, false)),
    );

    // Wire form of an observation: defaults fill title and layer.
    let parsed: WindowObservation = serde_json::from_str(r#"{"owner":"Zoom","bounds":null}"#)
        .unwrap_or_else(|_| window("", "", -1, None));
    check(
        "observation defaults",
        parsed.owner == "Zoom"
            && parsed.title.is_empty()
            && parsed.layer == 0
            && parsed.bounds.is_none(),
    );

    // Read-only live probe: the answer depends on the host and is not constrained.
    println!("ok: observe_os answered {}", observe_os().is_ok());

    println!("OK rightkit-presence 0.1.0 (registry consumer, public API)");
}
