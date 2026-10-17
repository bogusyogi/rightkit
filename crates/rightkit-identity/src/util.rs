//! Small crypto helpers. TODO(rightkit-crypto): replace with the shared owner once it is locked.

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let d = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..]);
    out
}

/// HMAC-SHA256 (RFC 2104).
pub(crate) fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(&ih[..]);
    let oh = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&oh[..]);
    out
}

/// Constant-time equality over equal-length inputs; a length mismatch returns false at once
/// (lengths are public).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const T: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(T[(b >> 4) as usize] as char);
        s.push(T[(b & 15) as usize] as char);
    }
    s
}

pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    s.as_bytes()
        .chunks(2)
        .map(|p| Some(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], crate::Error> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| crate::Error::Random)?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hex_round_trip_and_ct_eq() {
        let v = vec![0u8, 1, 254, 255];
        assert_eq!(unhex(&hex(&v)), Some(v.clone()));
        assert_eq!(unhex("zz"), None);
        assert!(ct_eq(&v, &v));
        assert!(!ct_eq(&v, &[0, 1, 254]));
        assert!(!ct_eq(&v, &[0, 1, 254, 0]));
    }
}
