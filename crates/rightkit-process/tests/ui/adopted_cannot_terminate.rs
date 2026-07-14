use std::num::NonZeroU32;

use rightkit_process::AdoptedProcess;

fn main() {
    let mut process = AdoptedProcess::new(NonZeroU32::new(42).unwrap());
    process.terminate_tree().unwrap();
}
