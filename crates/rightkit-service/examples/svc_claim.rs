//! Spawn-claim racer: `svc_claim <suite-root> <service> <go-file> <hold-ms>`.
//! Waits for the go file, tries `SpawnClaim::acquire`, prints `OWNER` or `HELD`,
//! and keeps running for `hold-ms` (an owner holds the claim meanwhile).
use rightkit_service::{SpawnClaim, SpawnClaimError, Suite};
use std::io::Write;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let suite = Suite::at(&a[0]).expect("suite");
    while !std::path::Path::new(&a[2]).exists() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let hold = std::time::Duration::from_millis(a[3].parse().unwrap_or(1000));
    let claim = SpawnClaim::acquire(&suite, &a[1]);
    match &claim {
        Ok(_) => println!("OWNER"),
        Err(SpawnClaimError::AlreadyHeld(_)) => println!("HELD"),
        Err(e) => println!("ERR {e}"),
    }
    let _ = std::io::stdout().flush();
    std::thread::sleep(hold);
}
