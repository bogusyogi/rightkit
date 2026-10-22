// rightkit-device 0.4.0 consumed by exact registry pin, outside the workspace.
//
// Proves device_id_hash, the only form of the device id that leaves the machine: the FIPS 180-4
// SHA-256 vector for "abc" is returned exactly, the output is 64 lowercase hex characters, the
// same raw id always gives the same hash, and no trimming happens (a trailing space changes the
// digest). Public API only.
// Does not prove: DeviceService, SecureService, the licence and authorization services, or the
// log and diagnostic hosts.
// State: pure string hashing. No network, no files, no process spawn, no user data.
use rightkit_device::device_id_hash;

fn main() {
    // Standard FIPS 180-4 test vector: SHA-256("abc").
    assert_eq!(
        device_id_hash("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "SHA-256 of abc"
    );

    let hash = device_id_hash("fixture-device-0001");
    assert_eq!(hash.len(), 64, "hash is 64 hex characters");
    assert!(
        hash.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "hash is lowercase hex"
    );
    assert_eq!(
        hash,
        device_id_hash("fixture-device-0001"),
        "same raw id, same hash"
    );
    assert_ne!(
        hash,
        device_id_hash("fixture-device-0001 "),
        "no trimming: a trailing space changes the digest"
    );

    println!("OK rightkit-device 0.4.0 (registry consumer)");
}
