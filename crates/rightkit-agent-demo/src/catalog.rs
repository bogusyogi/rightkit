//! The v1 demo catalog and the apps that can never be a demo target.
//!
//! The catalog is the only list of apps an agent may ask to drive. The agent names an entry by its
//! logical `id` (the macOS bundle id, the same on both OSes); the host maps it to the OS identifier
//! the automation allowlist keys on (bundle id on macOS, lowercase exe name on Windows).
//!
//! Every entry is declared capture-safe (design decision 4: v1 uses `CaptureSafeState` for the
//! Right Suite apps). RightKit has no runtime probe for an app's capture state, so the declaration is
//! the only signal; an app that is not in this table cannot be requested at all.

use rightkit_agent_tools::backends::demo::FIXTURE_APP_ID;

pub struct CatalogEntry {
    /// Logical id the agent passes in `demo.request` (the macOS bundle id).
    pub id: &'static str,
    pub name: &'static str,
    pub mac_id: &'static str,
    /// Lowercase exe file name.
    pub win_id: &'static str,
}

impl CatalogEntry {
    /// The identifier the automation allowlist and the recorder source list use on this OS.
    pub fn platform_id(&self) -> &'static str {
        if cfg!(windows) {
            self.win_id
        } else {
            self.mac_id
        }
    }
}

/// HeardRight is deliberately absent: its dictation and delivery surfaces are never agent-facing
/// (rightkit-agent-tools `exposure`), and driving its UI would sit next to them.
/// The Windows exe names are the apps' main binary names and are unverified on a Windows host.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "app.orthiclabs.genright",
        name: "GenRight",
        mac_id: "app.orthiclabs.genright",
        win_id: "genright.exe",
    },
    CatalogEntry {
        id: "app.orthiclabs.postright",
        name: "PostRight",
        mac_id: "app.orthiclabs.postright",
        win_id: "postright.exe",
    },
    // The released-build acceptance fixture. It exists only under the `demo` feature, like every
    // other line of this module.
    CatalogEntry {
        id: FIXTURE_APP_ID,
        name: "Demo fixture",
        mac_id: FIXTURE_APP_ID,
        win_id: "demo-fixture.exe",
    },
];

/// Suite apps the demo can never drive, whatever the catalog says. The host product itself
/// ([`crate::DemoIdentity::own_ids`]) is denied as well. These go into `Policy::deny`, which is checked
/// before the allow list. The host process is also protected by pid.
const DENIED: &[(&str, &str)] = &[("dev.orthic.pulse.hub", "pulse-hub.exe")];

pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

/// OS identifiers to pass to `Policy::deny`: both OSes' spellings, so a mismatch cannot open a gap.
pub fn denied_ids() -> impl Iterator<Item = &'static str> {
    let own = &crate::identity().own_ids;
    DENIED
        .iter()
        .flat_map(|(mac, win)| [*mac, *win])
        .chain([own.0.as_str(), own.1.as_str()])
}
