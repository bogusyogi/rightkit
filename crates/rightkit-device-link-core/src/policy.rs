//! Who a paired device may be, for the chat link: the receiving computer decides per call from the
//! caller's registry record.

use rightkit_device_link_types::{DevicePlatform, EffectClass, MemberRole, PairedDevice, Registry};

use super::circle::CheckKeyExt;

/// The owner check admitted this record's device (the non-guest "owner" rule of `authorize`).
fn owner_admitted(record: &PairedDevice) -> bool {
    !record.admit_denied
        && (record.admit
            || record.admit_check_key.as_ref().is_some_and(|k| {
                k.bound_to(&record.device.device_id, &record.public_key)
            })
            || record.endorsement.is_some()
            || record.grant_pair.is_some()
            // A host key counts only once it is bound to the host device (registry.rs
            // `pairing_check`, circle.rs `record_check_key`); an unbound key vouches for nobody.
            || record.pairing_check.as_ref().is_some_and(|k| {
                k.bound_to(&record.device.device_id, &record.public_key)
            }))
}

/// Owner-pairing questions a host asks of a [`PairedDevice`], including the phone rule that reads
/// this computer's enrolled approval keys.
pub trait OwnerPairedExt {
    /// The owner check admitted this device: `admit`, a trusted owner-check key bound to it, an
    /// endorsement, a join-token pairing, or a host key bound to the host device; and the owner has
    /// not taken `admit` from it. A guest is not looked at here: callers test `is_guest` first,
    /// because `authorize` calls a guest "owner".
    fn owner_admitted(&self) -> bool;

    /// Whether this device is one the OWNER paired, for owner-only asks and a phone's inbox and ask
    /// methods. Read from the registry record and this computer's own approval-key store, never from
    /// the device's own word, and never true for a guest or a revoked or pending record.
    /// `admin_root` is the device-link folder (the one that holds `registry.json` and the local
    /// revocations), where `admin_sig` looks for the phone's approval key.
    ///
    /// * A phone (`ios`, `android`) is owner-paired when its pairing is active, it is not a guest,
    ///   and it holds an enrolled owner approval key at this computer
    ///   (`admin_sig::enrolled_for`: a live key that has not been revoked here). A pairing alone
    ///   does not make a phone the owner's. A key store that cannot be read or does not answer is
    ///   "not enrolled" (fail closed). A phone never holds `admit`.
    /// * Any other device: the role is `computer` (a server or an unclassified computer is not) and
    ///   the owner check admitted it ([`Self::owner_admitted`]; the guest test comes first). No
    ///   approval key is asked of a computer.
    fn is_owner_paired(&self, admin_root: &std::path::Path) -> bool;
}

impl OwnerPairedExt for PairedDevice {
    fn owner_admitted(&self) -> bool {
        owner_admitted(self)
    }

    fn is_owner_paired(&self, admin_root: &std::path::Path) -> bool {
        if !self.is_active() || self.is_guest() {
            return false;
        }
        if matches!(
            self.device.platform,
            DevicePlatform::Ios | DevicePlatform::Android
        ) {
            return rightkit_owner_approvals::admin_sig::enrolled_for(
                admin_root,
                &self.device.device_id,
            )
            .unwrap_or(false);
        }
        self.member_role() == MemberRole::Computer && owner_admitted(self)
    }
}

/// Whether `record` is a device the owner paired as their own: active (not revoked, and not a
/// lapsed guest pass), not a guest, and admitted by the owner check (`admit`, a bound owner-check
/// key, an endorsement, a join-token grant, or a bound host key; never once `admit_denied`).
///
/// This is the source for the approvals `Authority` host's owner-paired fact. It is not
/// `authorize(..) == Ok("owner")`: `authorize` answers "owner" for a guest too (a chat rule label
/// for "full chat member", #492), and a guest must never count as an owner-paired device here. A
/// reversible grant alone never counts either. Pure over the record; takes no clock.
pub fn is_owner_paired(record: &PairedDevice) -> bool {
    record.is_unrevoked() && record.is_active() && !record.is_guest() && owner_admitted(record)
}

/// `is_owner_paired` for the peer with `device_id` in `registry`; false when the registry has no
/// such device. When a device id appears more than once, every record must qualify.
pub fn is_owner_paired_in(registry: &Registry, device_id: &str) -> bool {
    let mut found = false;
    for d in registry
        .devices
        .iter()
        .filter(|d| d.device.device_id == device_id)
    {
        if !is_owner_paired(d) {
            return false;
        }
        found = true;
    }
    found
}

/// Why [`verify_owner_paired_origin`] refused: a stable `code` and a sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginRefusal {
    pub code: &'static str,
    pub message: String,
}

/// Checks that a task's origin is a paired device the OWNER owns, read from the registry file at
/// `registry_path` now. `what` names the request kind in the sentences (for example `agent.ask`).
/// `is_paired_device` is whether the origin principal is the paired-device kind; `subject` is its
/// device id and `connection` its pairing id.
///
/// * The origin must be the paired-device principal, else `unsupported_origin`.
/// * No registry path known, or a registry that cannot be read: `owner_unknown` (fail closed).
/// * The record is found by device id AND the pairing the principal carries (`connection` is the
///   pairing id), so a device that was revoked and paired again is not the device that asked; it
///   must be [`OwnerPairedExt::is_owner_paired`] against the folder holding the registry. Else
///   `not_owner_paired` (a guest, a grant holder or an unknown device).
pub fn verify_owner_paired_origin(
    registry_path: Option<&std::path::Path>,
    is_paired_device: bool,
    subject: &str,
    connection: Option<&str>,
    what: &str,
) -> Result<(), OriginRefusal> {
    let refuse = |code: &'static str, message: String| Err(OriginRefusal { code, message });
    if !is_paired_device {
        return refuse(
            "unsupported_origin",
            format!("{what} is accepted only from a paired device the owner owns"),
        );
    }
    let Some(path) = registry_path else {
        return refuse(
            "owner_unknown",
            "the device registry is not known here, so the asking device cannot be checked; nothing was accepted"
                .to_string(),
        );
    };
    // The device-link folder: where the registry, the local revocations and the phone approval keys' state live.
    let admin_root = path
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let Ok(registry) = crate::registry::RegistryStore::new(path.to_path_buf()).load() else {
        return refuse(
            "owner_unknown",
            "the device registry could not be read, so the asking device cannot be checked; nothing was accepted"
                .to_string(),
        );
    };
    let owned = registry.devices.iter().any(|d| {
        d.device.device_id == subject
            && connection == Some(d.pairing_id.as_str())
            && d.is_owner_paired(&admin_root)
    });
    if owned {
        Ok(())
    } else {
        refuse(
            "not_owner_paired",
            format!(
                "{what} is accepted only from a device the owner paired (not a guest, a grant holder or an unknown device)"
            ),
        )
    }
}

/// Whether the paired computer `record` may use chat here, and by which rule. Ok("owner"): the
/// owner's own computer, one the owner check admitted (`admit`, a trusted owner-check key, an
/// endorsement) or joined through an owner-check token (`grant_pair`), and the owner has not taken `admit` from it. Ok("grant"): an unexpired `reversible`
/// grant. Otherwise the reason it is refused. Pairing alone grants nothing.
pub fn authorize(record: &PairedDevice, now_ms: u64) -> Result<&'static str, String> {
    if !record.is_active() {
        return Err("that device is no longer paired".to_string());
    }
    // A cloud agent the owner approved works for them like their other computers.
    if record.is_guest() {
        return Ok("owner");
    }
    if owner_admitted(record) {
        return Ok("owner");
    }
    let unexpired = record.grants_expire_at_ms.is_none_or(|at| at > now_ms);
    if unexpired && record.grants.contains(&EffectClass::Reversible) {
        return Ok("grant");
    }
    Err(
        "this computer does not take chat from that device: it is not one the owner check \
         admitted and holds no reversible grant (the app's device settings)"
            .to_string(),
    )
}
