//! rightkit-device: the device-id digest that leaves the machine is the FIPS 180-4 SHA-256 of the raw id (checked against the
//! standard test vector), lowercase hex, stable, and never the raw id.
use rightkit_device::device_id_hash;
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let abc = device_id_hash("abc");
    if abc != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
        return Err(format!("SHA-256(\"abc\") came out {abc}"));
    }
    let raw = "device-raw-id-1234";
    let h = device_id_hash(raw);
    if h != device_id_hash(raw) || h.contains(raw) || h.len() != 64 || h.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("the digest is not a stable lowercase 64-hex value: {h}"));
    }
    Ok(json!({ "abc": abc, "len": h.len() }))
}
