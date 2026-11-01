//! Who a paired device may be, for the chat link: the receiving computer decides per call from the
//! caller's registry record.

use rightkit_device_link_types::{EffectClass, PairedDevice};

use super::circle::CheckKeyExt;

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
    let owner = !record.admit_denied
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
            }));
    if owner {
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
