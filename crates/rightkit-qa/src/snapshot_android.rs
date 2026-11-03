//! The Android part of the snapshot and restore of the owner's real data (rightkit#1437).
//!
//! An Android journey runs on the dev package on an emulator or on the owner's device, with the
//! app's real data. That data is the package's private data dir, which only the package and
//! `run-as` (a debuggable build) can read. [`snapshot`] streams `tar -cf - .` of that dir through
//! `adb exec-out run-as <package>` into a file of the snapshot folder, lists every path and file
//! hash of the dir on the device, and re-lists it to prove the data did not change while it was
//! copied. [`restore`] re-hashes the file against the marker before the first device write,
//! force-stops the package, empties the data dir, unpacks the tar through
//! `adb shell run-as <package> tar -xf -`, and lists the dir again: the listing must equal the one
//! taken at snapshot time, or the restore fails and the marker stays.
//!
//! Only a [`TestPackage`] is ever named (the same rule as the Android driver), the serial is
//! required (no "the only attached device" shortcut), and nothing here uninstalls anything: an
//! uninstall deletes the app's data. The top-level `lib` entry is left out of the copy, the listing
//! and the empty-out: on some Android versions it is a symlink to the installed APK's native
//! libraries, whose path changes when the package is reinstalled, and putting an old target back
//! would break the app.

use crate::android::{
    args_force_stop, args_pidof, args_run_as_probe, list_devices, parse_pids, parse_run_as,
    resolve_adb_from_env, select_device, sh_quote, AdbRunner, ProcessRunner, TestPackage,
    ADB_TIMEOUT, TEARDOWN_TIMEOUT,
};
use crate::config::AndroidSnapshotConfig;
use crate::util::{err, sha256_hex, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The tar of the app's data dir, inside the snapshot folder.
const TAR_FILE: &str = "android-data.tar";
/// Every path and file hash of the data dir at snapshot time, inside the snapshot folder.
const LISTING_FILE: &str = "android-listing.txt";
/// A whole data dir (models included) can be large and `adb` is slow over USB.
const COPY_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const LIST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What the marker keeps of the Android snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AndroidRecord {
    pub package: String,
    pub serial: String,
    pub adb: Option<String>,
    pub segments: Vec<String>,
    /// sha256 of [`TAR_FILE`], set when the copy is verified.
    pub tar_sha256: Option<String>,
    /// sha256 of [`LISTING_FILE`], set when the copy is verified.
    pub listing_sha256: Option<String>,
}

impl AndroidRecord {
    pub(crate) fn from_config(cfg: &AndroidSnapshotConfig) -> AndroidRecord {
        AndroidRecord {
            package: cfg.package.clone(),
            serial: cfg.serial.clone(),
            adb: cfg.adb.clone(),
            segments: cfg.segments.clone(),
            tar_sha256: None,
            listing_sha256: None,
        }
    }
}

/// Check an `[snapshot.android]` table without touching a device: the package must be a test
/// package and the serial must be a plausible one.
pub(crate) fn validate(cfg: &AndroidSnapshotConfig) -> Result<()> {
    package_of(&cfg.package, &cfg.segments)?;
    if cfg.serial.trim().is_empty() {
        return err("[snapshot.android].serial is required: the snapshot never picks \"the only attached device\", so it cannot mistake the owner's phone for the emulator");
    }
    Ok(())
}

fn package_of(name: &str, segments: &[String]) -> Result<TestPackage> {
    if segments.is_empty() {
        TestPackage::new(name)
    } else {
        let refs: Vec<&str> = segments.iter().map(String::as_str).collect();
        TestPackage::with_segments(name, &refs)
    }
}

struct Device {
    runner: Box<dyn AdbRunner>,
    serial: String,
    package: TestPackage,
}

impl Device {
    fn open(rec: &AndroidRecord) -> Result<Device> {
        let adb = match &rec.adb {
            Some(p) => PathBuf::from(p),
            None => resolve_adb_from_env()?,
        };
        let runner: Box<dyn AdbRunner> = Box::new(ProcessRunner::new(adb));
        let devices = list_devices(runner.as_ref())?;
        let device = select_device(&devices, &rec.serial)?;
        let package = package_of(&rec.package, &rec.segments)?;
        let this = Device {
            runner,
            serial: device.serial,
            package,
        };
        // Only a debuggable build answers `run-as`; a release build's data cannot be reached, and
        // a snapshot that cannot read the data is not one.
        let probe = this.call(args_run_as_probe(&this.package), ADB_TIMEOUT)?;
        parse_run_as(&probe.text()).map_err(|e| {
            Error(format!(
                "{}: the snapshot needs a debuggable build of {} installed on {}",
                e.0,
                this.package.as_str(),
                this.serial
            ))
        })?;
        Ok(this)
    }

    fn full(&self, args: Vec<String>) -> Vec<String> {
        let mut full = vec!["-s".to_string(), self.serial.clone()];
        full.extend(args);
        full
    }

    fn call(&self, args: Vec<String>, timeout: Duration) -> Result<crate::android::AdbOutput> {
        self.runner.run(&self.full(args), timeout)
    }

    fn running(&self) -> Result<bool> {
        let out = self.call(args_pidof(&self.package), ADB_TIMEOUT)?;
        Ok(!parse_pids(&out.stdout_text()).is_empty())
    }

    /// `adb shell run-as <package> sh -c '<script>'`, run in the package's data dir.
    fn script_args(&self, script: &str) -> Vec<String> {
        vec![
            "shell".to_string(),
            "run-as".to_string(),
            self.package.as_str().to_string(),
            "sh".to_string(),
            "-c".to_string(),
            sh_quote(script),
        ]
    }

    /// The sorted path list and `sha256sum` lines of the data dir, minus sockets and the
    /// top-level `lib` entry.
    fn listing(&self) -> Result<String> {
        let script = "find . ! -type s ! -path ./lib ! -path './lib/*' | sort; echo ---; \
                      find . -type f ! -path './lib/*' -exec sha256sum {} + | sort -k 2";
        let out = self.call(self.script_args(script), LIST_TIMEOUT)?;
        let text = out.stdout_text();
        if !text.contains("\n---\n") && !text.starts_with("---\n") {
            return err(format!(
                "cannot list the data dir of {} on {}: {}",
                self.package.as_str(),
                self.serial,
                first(&out.text())
            ));
        }
        Ok(text.replace("\r\n", "\n"))
    }
}

fn first(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no output)")
        .chars()
        .take(200)
        .collect()
}

/// Copy the package's data dir into `snap_dir` and record its hashes in `rec`. Refuses while the
/// package runs on the device. Nothing on the device is changed.
pub(crate) fn snapshot(rec: &mut AndroidRecord, snap_dir: &Path) -> Result<()> {
    let device = Device::open(rec)?;
    if device.running()? {
        return err(format!(
            "{} is running on {}; stop it (or quit the app) before a journey run. Nothing was changed.",
            device.package.as_str(),
            device.serial
        ));
    }
    let before = device.listing()?;
    let dir = snap_dir.join("android");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error(format!("cannot create {}: {e}", dir.display())))?;
    let tar_path = dir.join(TAR_FILE);
    let file = File::create(&tar_path)
        .map_err(|e| Error(format!("cannot create {}: {e}", tar_path.display())))?;
    let args = device.full(vec![
        "exec-out".to_string(),
        "run-as".to_string(),
        device.package.as_str().to_string(),
        "tar".to_string(),
        "-cf".to_string(),
        "-".to_string(),
        "--exclude=./lib".to_string(),
        ".".to_string(),
    ]);
    let out = device.runner.run_to_file(&args, file, COPY_TIMEOUT)?;
    let size = std::fs::metadata(&tar_path).map(|m| m.len()).unwrap_or(0);
    if !out.success || size < 1024 {
        return err(format!(
            "tar of the data dir of {} on {} failed ({} bytes): {}",
            device.package.as_str(),
            device.serial,
            size,
            first(&out.text())
        ));
    }
    // The data must not have changed while it was copied.
    let after = device.listing()?;
    if before != after {
        return err(format!(
            "the data dir of {} on {} changed while it was copied (is the app still writing?)",
            device.package.as_str(),
            device.serial
        ));
    }
    let listing_path = dir.join(LISTING_FILE);
    std::fs::write(&listing_path, before.as_bytes())
        .map_err(|e| Error(format!("cannot write {}: {e}", listing_path.display())))?;
    rec.tar_sha256 = Some(
        rightkit_fs::digest::sha256_file(&tar_path)
            .map_err(|e| Error(format!("cannot hash {}: {e}", tar_path.display())))?,
    );
    rec.listing_sha256 = Some(sha256_hex(before.as_bytes()));
    Ok(())
}

/// Re-hash the copy in `snap_dir` against `rec`. The caller runs it before the first write to any
/// live data, so a damaged copy stops the restore with nothing touched.
pub(crate) fn verify_copy(rec: &AndroidRecord, snap_dir: &Path) -> Result<()> {
    let dir = snap_dir.join("android");
    let tar_path = dir.join(TAR_FILE);
    let listing_path = dir.join(LISTING_FILE);
    let tar_sha = rightkit_fs::digest::sha256_file(&tar_path)
        .map_err(|e| Error(format!("cannot hash {}: {e}", tar_path.display())))?;
    if Some(&tar_sha) != rec.tar_sha256.as_ref() {
        return err(
            "the Android snapshot does not match the restore marker; live data was NOT touched",
        );
    }
    let listing = std::fs::read(&listing_path)
        .map_err(|e| Error(format!("cannot read {}: {e}", listing_path.display())))?;
    if Some(sha256_hex(&listing)) != rec.listing_sha256 {
        return err(
            "the Android listing does not match the restore marker; live data was NOT touched",
        );
    }
    Ok(())
}

/// Put the data dir back to the snapshot and verify it. [`verify_copy`] has already passed.
pub(crate) fn restore(rec: &AndroidRecord, snap_dir: &Path) -> Result<()> {
    let dir = snap_dir.join("android");
    let tar_path = dir.join(TAR_FILE);
    let listing_path = dir.join(LISTING_FILE);
    let device = Device::open(rec)?;
    // Stop the package: files of a running app are being written while they are replaced.
    let stop = device.call(args_force_stop(&device.package), TEARDOWN_TIMEOUT)?;
    if !stop.success && device.running()? {
        return err(format!(
            "cannot stop {} on {} for the restore: {}",
            device.package.as_str(),
            device.serial,
            first(&stop.text())
        ));
    }
    // Empty the data dir (the top-level `lib` entry stays), then unpack.
    let wipe = device.call(
        device.script_args(
            "for f in ./* ./.[!.]* ./..?*; do case \"$f\" in ./lib) ;; *) rm -rf \"$f\" ;; esac; done",
        ),
        LIST_TIMEOUT,
    )?;
    if !wipe.success {
        return err(format!(
            "cannot empty the data dir of {} on {}: {}",
            device.package.as_str(),
            device.serial,
            first(&wipe.text())
        ));
    }
    let input = File::open(&tar_path)
        .map_err(|e| Error(format!("cannot open {}: {e}", tar_path.display())))?;
    let args = device.full(vec![
        "shell".to_string(),
        "run-as".to_string(),
        device.package.as_str().to_string(),
        "tar".to_string(),
        "-xf".to_string(),
        "-".to_string(),
    ]);
    let out = device.runner.run_from_file(&args, input, COPY_TIMEOUT)?;
    if !out.success {
        return err(format!(
            "cannot unpack the snapshot into the data dir of {} on {}: {}",
            device.package.as_str(),
            device.serial,
            first(&out.text())
        ));
    }
    let now = device.listing()?;
    if sha256_hex(now.as_bytes()) != rec.listing_sha256.clone().unwrap_or_default() {
        return err(format!(
            "the data dir of {} on {} does not match the snapshot after the restore ({})",
            device.package.as_str(),
            device.serial,
            diff_summary(
                &std::fs::read_to_string(&listing_path).unwrap_or_default(),
                &now
            )
        ));
    }
    Ok(())
}

/// At most 10 lines that differ between the two listings.
fn diff_summary(expected: &str, actual: &str) -> String {
    let ex: std::collections::BTreeSet<&str> = expected.lines().collect();
    let ac: std::collections::BTreeSet<&str> = actual.lines().collect();
    let mut out: Vec<String> = Vec::new();
    for l in ex.difference(&ac).take(5) {
        out.push(format!("missing or different: {l}"));
    }
    for l in ac.difference(&ex).take(5) {
        out.push(format!("extra or different: {l}"));
    }
    out.join("; ")
}
