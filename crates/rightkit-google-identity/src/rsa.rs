//! RSASSA-PKCS1-v1_5 / SHA-256 signature verification (RFC 8017 section 8.2.2), verification
//! only. Public values only, so the variable-time arithmetic leaks nothing secret. The expected
//! encoded message is rebuilt and compared whole, so no padding is ever parsed.

use sha2::{Digest, Sha256};

const SHA256_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// Smallest accepted modulus, in bits.
pub(crate) const MIN_MODULUS_BITS: usize = 2048;

fn from_be(bytes: &[u8], len: usize) -> Vec<u32> {
    let mut limbs = vec![0u32; len];
    for (i, b) in bytes.iter().rev().enumerate() {
        limbs[i / 4] |= (*b as u32) << (8 * (i % 4));
    }
    limbs
}

fn ge(a: &[u32], b: &[u32]) -> bool {
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn sub_in_place(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let v = a[i] as i64 - b[i] as i64 - borrow;
        a[i] = v as u32;
        borrow = (v < 0) as i64;
    }
}

/// Montgomery product `a * b * R^-1 mod n` (CIOS), inputs below `n`, `n` odd.
fn mont_mul(a: &[u32], b: &[u32], n: &[u32], n0inv: u32) -> Vec<u32> {
    let len = n.len();
    let mut t = vec![0u32; len + 2];
    for &bi in b.iter().take(len) {
        let mut carry = 0u64;
        for j in 0..len {
            let v = t[j] as u64 + a[j] as u64 * bi as u64 + carry;
            t[j] = v as u32;
            carry = v >> 32;
        }
        let v = t[len] as u64 + carry;
        t[len] = v as u32;
        t[len + 1] = (v >> 32) as u32;
        let m = t[0].wrapping_mul(n0inv);
        let v = t[0] as u64 + m as u64 * n[0] as u64;
        let mut carry = v >> 32;
        for j in 1..len {
            let v = t[j] as u64 + m as u64 * n[j] as u64 + carry;
            t[j - 1] = v as u32;
            carry = v >> 32;
        }
        let v = t[len] as u64 + carry;
        t[len - 1] = v as u32;
        t[len] = t[len + 1].wrapping_add((v >> 32) as u32);
        t[len + 1] = 0;
    }
    let mut out: Vec<u32> = t[..len].to_vec();
    if t[len] != 0 || ge(&out, n) {
        sub_in_place(&mut out, n);
    }
    out
}

/// `s^e mod n` with big-endian `e`; `s < n`, `n` odd.
fn mod_pow(s: &[u32], e: &[u8], n: &[u32]) -> Vec<u32> {
    let len = n.len();
    let mut n0inv = n[0];
    for _ in 0..5 {
        n0inv = n0inv.wrapping_mul(2u32.wrapping_sub(n[0].wrapping_mul(n0inv)));
    }
    let n0inv = n0inv.wrapping_neg();
    // R^2 mod n by doubling 1, 64 * len times.
    let mut r2 = vec![0u32; len];
    r2[0] = 1;
    for _ in 0..64 * len {
        let mut carry = 0u32;
        for limb in r2.iter_mut() {
            let next = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        if carry != 0 || ge(&r2, n) {
            sub_in_place(&mut r2, n);
        }
    }
    let base = mont_mul(s, &r2, n, n0inv);
    let mut acc: Option<Vec<u32>> = None;
    for byte in e {
        for bit in (0..8).rev() {
            if let Some(a) = acc.take() {
                acc = Some(mont_mul(&a, &a, n, n0inv));
            }
            if (byte >> bit) & 1 == 1 {
                acc = Some(match acc.take() {
                    Some(a) => mont_mul(&a, &base, n, n0inv),
                    None => base.clone(),
                });
            }
        }
    }
    let mut one = vec![0u32; len];
    one[0] = 1;
    mont_mul(&acc.unwrap_or(one.clone()), &one, n, n0inv)
}

fn strip(b: &[u8]) -> &[u8] {
    let i = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    &b[i..]
}

/// Verify an RS256 signature over `message`. `n` and `e` are big-endian.
pub(crate) fn verify_rs256(n: &[u8], e: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let n = strip(n);
    let e = strip(e);
    if n.is_empty() || n.len() * 8 - n[0].leading_zeros() as usize <= MIN_MODULUS_BITS - 1 {
        return false;
    }
    if n[n.len() - 1] & 1 == 0 || e.is_empty() || e.len() > 4 || (e[e.len() - 1] & 1) == 0 {
        return false;
    }
    if e.len() == 1 && e[0] < 3 {
        return false;
    }
    let k = n.len();
    if signature.len() != k {
        return false;
    }
    let limbs = k.div_ceil(4);
    let nl = from_be(n, limbs);
    let sl = from_be(signature, limbs);
    if ge(&sl, &nl) {
        return false;
    }
    let m = mod_pow(&sl, e, &nl);
    let mut em = vec![0u8; k];
    for (i, b) in em.iter_mut().rev().enumerate() {
        *b = (m[i / 4] >> (8 * (i % 4))) as u8;
    }
    let hash = Sha256::digest(message);
    let t_len = SHA256_PREFIX.len() + 32;
    if k < t_len + 11 {
        return false;
    }
    let mut expected = vec![0xffu8; k];
    expected[0] = 0;
    expected[1] = 1;
    expected[k - t_len - 1] = 0;
    expected[k - t_len..k - 32].copy_from_slice(&SHA256_PREFIX);
    expected[k - 32..].copy_from_slice(&hash);
    let mut diff = 0u8;
    for (a, b) in em.iter().zip(&expected) {
        diff |= a ^ b;
    }
    diff == 0
}
