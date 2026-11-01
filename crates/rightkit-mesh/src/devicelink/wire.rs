//! Canonical byte encoding for everything that is signed or hashed, plus hex and randomness.
//!
//! Signed bytes are never JSON. Each signed value starts with a domain string, so a signature
//! made for one message type cannot be replayed as another, and every field is length-prefixed
//! so two different field lists cannot encode to the same bytes.

use ring::rand::{SecureRandom, SystemRandom};

use super::LinkError;
use rightkit_device_link_types::{DevicePlatform, DeviceRef, EffectClass};

/// Length-prefixed, domain-separated byte builder.
pub(crate) struct Enc(Vec<u8>);

impl Enc {
    pub(crate) fn new(domain: &str) -> Self {
        Enc(Vec::new()).str(domain)
    }

    pub(crate) fn bytes(mut self, value: &[u8]) -> Self {
        self.0
            .extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.0.extend_from_slice(value);
        self
    }

    pub(crate) fn str(self, value: &str) -> Self {
        self.bytes(value.as_bytes())
    }

    pub(crate) fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// The identity-relevant fields of a device reference. `display_name` is bound too, so a
    /// confirmed pairing also confirms the label the user saw.
    pub(crate) fn device(self, device: &DeviceRef) -> Self {
        let this = self
            .u64(u64::from(device.schema_version))
            .str(&device.device_id)
            .str(platform_tag(device.platform));
        match &device.display_name {
            Some(name) => this.u64(1).str(name),
            None => this.u64(0),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.0
    }
}

pub(crate) fn platform_tag(platform: DevicePlatform) -> &'static str {
    match platform {
        DevicePlatform::Mac => "mac",
        DevicePlatform::Windows => "windows",
        DevicePlatform::Linux => "linux",
        DevicePlatform::Ios => "ios",
        DevicePlatform::Android => "android",
    }
}

pub(crate) fn effect_tag(effect: EffectClass) -> &'static str {
    match effect {
        EffectClass::Reversible => "reversible",
        EffectClass::IrreversibleDestruction => "irreversible-destruction",
        EffectClass::Spend => "spend",
        EffectClass::Publication => "publication",
    }
}

/// `N` bytes from the system RNG.
pub fn random<const N: usize>() -> Result<[u8; N], LinkError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| LinkError::Random)?;
    Ok(out)
}

/// Decode exactly `N` bytes of lowercase or uppercase hex.
pub fn unhex<const N: usize>(text: &str) -> Result<[u8; N], LinkError> {
    rightkit_fs::digest::unhex(text)
        .ok_or(LinkError::Malformed("hex"))?
        .try_into()
        .map_err(|_| LinkError::Malformed("hex length"))
}
