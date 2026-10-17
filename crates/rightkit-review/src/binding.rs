//! Binds a verdict to the checks and the policy that produced it (Orthic-Labs/rightkit#179).
//!
//! A verdict is only as good as the policy it was measured against. Reusing a stored pass when
//! only a preset *name* matches would accept a stale pass after the policy tightened. A
//! [`PolicyBinding`] carries the SHA-256 of the canonical rule set (id, version, every rule, its
//! thresholds and lists) and the check version of each rule. A record is reusable only when
//! [`crate::VerdictRecord::verify_current`] accepts it against the rule set in force now.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::rules::{RuleKind, RuleSet};
use crate::ReviewError;

/// Version of the built-in check implementations in this crate. Bump it when any built-in check
/// changes how it measures or decides (not when only a threshold in the policy changes: the
/// policy hash covers that). Every stored verdict from an older value is stale.
pub const BUILTIN_CHECK_VERSION: u32 = 1;

/// The check version recorded for an extension rule. Its implementation belongs to the
/// consumer, so this crate cannot version it; its kind name and params are inside the policy
/// hash, and a consumer that changes the code bumps `RuleSet::version` or the params.
pub const EXTENSION_CHECK_VERSION: u32 = 0;

const POLICY_HASH_DOMAIN: &str = "rightkit-review.policy.v1\n";

/// The check versions and policy hash a verdict was produced under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyBinding {
    /// Lowercase hex SHA-256 over the canonical JSON of the whole rule set.
    pub policy_hash: String,
    /// Check version per rule id.
    pub check_versions: BTreeMap<String, u32>,
}

/// The check version of one rule kind.
pub fn check_version(kind: &RuleKind) -> u32 {
    match kind {
        RuleKind::Extension { .. } => EXTENSION_CHECK_VERSION,
        _ => BUILTIN_CHECK_VERSION,
    }
}

impl RuleSet {
    /// The binding of this rule set as it stands now.
    pub fn policy_binding(&self) -> Result<PolicyBinding, ReviewError> {
        let value = serde_json::to_value(self)?;
        let mut canonical = String::from(POLICY_HASH_DOMAIN);
        write_canonical(&value, &mut canonical)?;
        Ok(PolicyBinding {
            policy_hash: sha256_hex(canonical.as_bytes()),
            check_versions: self
                .rules
                .iter()
                .map(|rule| (rule.id.clone(), check_version(&rule.kind)))
                .collect(),
        })
    }

    /// The policy hash alone.
    pub fn policy_hash(&self) -> Result<String, ReviewError> {
        Ok(self.policy_binding()?.policy_hash)
    }
}

/// Canonical JSON: object keys sorted, no whitespace, numbers and strings as `serde_json`
/// writes them. Independent of whether `serde_json` keeps map insertion order.
pub(crate) fn write_canonical(
    value: &serde_json::Value,
    out: &mut String,
) -> Result<(), ReviewError> {
    match value {
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
        scalar => out.push_str(&serde_json::to_string(scalar)?),
    }
    Ok(())
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4), lowercase hex. Local so the crate keeps no hashing dependency.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(add);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_matches_the_fips_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
