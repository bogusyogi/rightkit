//! Who a paired device may be, for the chat link: the receiving computer decides per call from the
//! caller's registry record.

use rightkit_device_link_types::{EffectClass, PairedDevice, Registry};

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
    for d in registry.devices.iter().filter(|d| d.device.device_id == device_id) {
        if !is_owner_paired(d) {
            return false;
        }
        found = true;
    }
    found
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
