//! Tart clean-machine VM lifecycle for macOS QA (#442): clone a base image, run it (headless or
//! with a window), reach it over ssh, copy files in and out, and destroy it. Wraps the `tart`,
//! `ssh` and `scp` command lines; the caller supplies the tart binary path. Bounded commands run
//! through [`run_owned`], so they are tracked and killed on timeout. A VM that is dropped without
//! [`CleanVm::destroy`] is stopped and deleted on a best-effort basis.
use crate::process::{run_owned, Output, RunOptions, Tracker};
use crate::util::{err, Error, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// One cloned VM. Its name is the clone name, not the base image.
pub struct CleanVm {
    tart: PathBuf,
    name: String,
    ssh_user: String,
    run: Option<Child>,
    destroyed: bool,
}

fn bounded(
    tracker: &Tracker,
    label: &str,
    program: &str,
    args: Vec<String>,
    secs: u64,
) -> Result<Output> {
    run_owned(
        program,
        &args,
        &RunOptions {
            timeout: Some(Duration::from_secs(secs)),
            label: label.into(),
            ..Default::default()
        },
        tracker,
    )
}

fn ok_or_err(label: &str, out: Output) -> Result<Output> {
    if out.timed_out {
        return err(format!("{label} timed out"));
    }
    if out.code != Some(0) {
        return err(format!(
            "{label} failed ({:?}): {}",
            out.code,
            crate::util::tail(&out.stderr, 500)
        ));
    }
    Ok(out)
}

impl CleanVm {
    /// `tart clone <base> <name>`: a fresh VM from the base image. `ssh_user` is the account
    /// [`CleanVm::ssh`] and the copies log in as.
    pub fn clone_from(
        tart: &Path,
        base: &str,
        name: &str,
        ssh_user: &str,
        tracker: &Tracker,
    ) -> Result<CleanVm> {
        let tart_s = tart.display().to_string();
        ok_or_err(
            "tart clone",
            bounded(
                tracker,
                "tart clone",
                &tart_s,
                vec!["clone".into(), base.into(), name.into()],
                1800,
            )?,
        )?;
        Ok(CleanVm {
            tart: tart.to_path_buf(),
            name: name.to_string(),
            ssh_user: ssh_user.to_string(),
            run: None,
            destroyed: false,
        })
    }

    /// The clone name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `tart run [--no-graphics] <name>` as a child this VM owns. Fails if it exits at once.
    pub fn run(&mut self, headless: bool, tracker: &Tracker) -> Result<()> {
        if self.run.is_some() {
            return err(format!("VM {} is already running", self.name));
        }
        // windowless: tart is a macOS-only VM tool; this code never runs on Windows
        let mut cmd = Command::new(&self.tart);
        cmd.arg("run");
        if headless {
            cmd.arg("--no-graphics");
        }
        cmd.arg(&self.name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| Error(format!("cannot start tart run: {e}")))?;
        std::thread::sleep(Duration::from_millis(500));
        if let Some(status) = child.try_wait()? {
            return err(format!("tart run exited at once ({status})"));
        }
        tracker.register(child.id(), &format!("tart-run:{}", self.name));
        self.run = Some(child);
        Ok(())
    }

    /// The VM's IP address, waiting up to `wait` for it (`tart ip --wait`).
    pub fn ip(&self, wait: Duration, tracker: &Tracker) -> Result<String> {
        let out = ok_or_err(
            "tart ip",
            bounded(
                tracker,
                "tart ip",
                &self.tart.display().to_string(),
                vec![
                    "ip".into(),
                    self.name.clone(),
                    "--wait".into(),
                    wait.as_secs().max(1).to_string(),
                ],
                wait.as_secs().max(1) + 30,
            )?,
        )?;
        let ip = out.stdout.trim().to_string();
        if ip.is_empty() {
            return err(format!("tart ip printed no address for {}", self.name));
        }
        Ok(ip)
    }

    fn ssh_args(&self, ip: &str) -> Vec<String> {
        vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "ConnectTimeout=20".into(),
            format!("{}@{ip}", self.ssh_user),
        ]
    }

    /// Run `cmd` on the VM over ssh and return its output (the exit code is in `code`; a non-zero
    /// code is returned, not an error).
    pub fn ssh(&self, cmd: &str, tracker: &Tracker) -> Result<Output> {
        let ip = self.ip(Duration::from_secs(120), tracker)?;
        let mut args = self.ssh_args(&ip);
        args.push(cmd.to_string());
        bounded(tracker, "ssh", "ssh", args, 900)
    }

    /// Copy a local file or folder into the VM (`scp -r`). `remote` is a path on the VM.
    pub fn copy_in(&self, local: &Path, remote: &str, tracker: &Tracker) -> Result<()> {
        let ip = self.ip(Duration::from_secs(120), tracker)?;
        let from = local.display().to_string();
        let to = format!("{}@{ip}:{remote}", self.ssh_user);
        self.scp(from, to, tracker)
    }

    /// Copy a file or folder out of the VM (`scp -r`). `remote` is a path on the VM.
    pub fn copy_out(&self, remote: &str, local: &Path, tracker: &Tracker) -> Result<()> {
        let ip = self.ip(Duration::from_secs(120), tracker)?;
        let from = format!("{}@{ip}:{remote}", self.ssh_user);
        self.scp(from, local.display().to_string(), tracker)
    }

    fn scp(&self, from: String, to: String, tracker: &Tracker) -> Result<()> {
        let mut args = vec![
            "-r".to_string(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
        ];
        args.push(from);
        args.push(to);
        ok_or_err("scp", bounded(tracker, "scp", "scp", args, 1800)?)?;
        Ok(())
    }

    /// Stop the VM, delete the clone and end its run child. Reports a failed delete.
    pub fn destroy(mut self, tracker: &Tracker) -> Result<()> {
        self.stop_run(tracker);
        let _ = bounded(
            tracker,
            "tart stop",
            &self.tart.display().to_string(),
            vec!["stop".into(), self.name.clone()],
            120,
        );
        ok_or_err(
            "tart delete",
            bounded(
                tracker,
                "tart delete",
                &self.tart.display().to_string(),
                vec!["delete".into(), self.name.clone()],
                300,
            )?,
        )?;
        self.destroyed = true;
        Ok(())
    }

    fn stop_run(&mut self, tracker: &Tracker) {
        if let Some(mut child) = self.run.take() {
            tracker.forget(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for CleanVm {
    fn drop(&mut self) {
        if self.destroyed {
            return;
        }
        if let Some(mut child) = self.run.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let tart = self.tart.display().to_string();
        let tracker = Tracker::new();
        let _ = bounded(
            &tracker,
            "tart stop",
            &tart,
            vec!["stop".into(), self.name.clone()],
            120,
        );
        let _ = bounded(
            &tracker,
            "tart delete",
            &tart,
            vec!["delete".into(), self.name.clone()],
            300,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_arguments_name_the_user_and_address_and_never_prompt() {
        let vm = CleanVm {
            tart: PathBuf::from("/usr/local/bin/tart"),
            name: "qa-1".into(),
            ssh_user: "admin".into(),
            run: None,
            destroyed: true,
        };
        let args = vm.ssh_args("192.168.64.9");
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert_eq!(args.last().unwrap(), "admin@192.168.64.9");
        assert_eq!(vm.name(), "qa-1");
    }

    #[test]
    fn dropping_a_vm_with_a_missing_tart_binary_returns_promptly() {
        let started = std::time::Instant::now();
        drop(CleanVm {
            tart: PathBuf::from("/nonexistent/tart"),
            name: "qa-drop".into(),
            ssh_user: "admin".into(),
            run: None,
            destroyed: false,
        });
        assert!(started.elapsed() < Duration::from_secs(30));
    }
}
