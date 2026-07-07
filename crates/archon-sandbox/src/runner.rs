use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};

use crate::policy::SandboxPolicy;

/// Which half of the two-stage build is running. See the crate-level docs
/// for why the split exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStage {
    Fetch,
    Build,
}

impl BuildStage {
    fn makepkg_args(self) -> &'static [&'static str] {
        match self {
            // Download + extract + run prepare(); stop before build().
            BuildStage::Fetch => &["--nobuild", "--nodeps", "--noconfirm"],
            // Reuse the already-extracted $srcdir; run build() + package().
            BuildStage::Build => &["--noextract", "--nodeps", "--noconfirm", "--force"],
        }
    }

    fn allow_network(self) -> bool {
        matches!(self, BuildStage::Fetch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("bubblewrap (bwrap) not found in PATH — install the `bubblewrap` package")]
    BwrapNotFound,
    #[error("failed to launch bwrap: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("{stage:?} stage failed: makepkg exited with {status}")]
    BuildFailed { stage: BuildStage, status: ExitStatus },
}

/// Drives one AUR package's build through bubblewrap.
pub struct SandboxRunner {
    pkg_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    run_as: Option<(u32, u32)>,
}

impl SandboxRunner {
    pub fn new(pkg_dir: impl Into<PathBuf>) -> Self {
        Self {
            pkg_dir: pkg_dir.into(),
            extra_env: Vec::new(),
            run_as: None,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Drops privileges to `(uid, gid)` before `bwrap` execs, clearing
    /// supplementary groups down to just `gid` first. For the one caller
    /// that needs this: archon running as root (under `sudo`, to load the
    /// eBPF monitor) must still spawn the actual build as the invoking
    /// user — `makepkg` refuses outright to run as EUID 0.
    pub fn run_as(mut self, uid: u32, gid: u32) -> Self {
        self.run_as = Some((uid, gid));
        self
    }

    pub fn bwrap_available() -> bool {
        Command::new("bwrap")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Spawn one stage under bwrap without waiting on it — lets a caller
    /// (the Phase 3 monitor) observe the child's PID before the build runs,
    /// so no syscalls happen unobserved.
    pub fn spawn_stage(&self, stage: BuildStage) -> Result<Child, SandboxError> {
        if !Self::bwrap_available() {
            return Err(SandboxError::BwrapNotFound);
        }
        let policy = SandboxPolicy {
            pkg_dir: self.pkg_dir.clone(),
            allow_network: stage.allow_network(),
            extra_env: self.extra_env.clone(),
        };
        let mut command: Vec<String> = vec!["makepkg".into()];
        command.extend(stage.makepkg_args().iter().map(|s| s.to_string()));
        let args = policy.bwrap_args(&command);
        let mut cmd = Command::new("bwrap");
        cmd.args(&args);
        if let Some((uid, gid)) = self.run_as {
            use std::os::unix::process::CommandExt;
            // `Command::uid`/`gid` alone would leave root's supplementary
            // groups attached to the child; `Command::groups` (which would
            // clear them) is still unstable. Do the whole drop ourselves,
            // in the only safe order: groups, then gid, then uid — dropping
            // uid first would leave us without permission to change the
            // others. pre_exec runs in the forked child before exec, so
            // only async-signal-safe calls (raw syscalls, no allocation)
            // belong here — setgroups/setgid/setuid qualify.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::setgroups(1, &gid as *const u32) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setgid(gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setuid(uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd.spawn().map_err(SandboxError::Spawn)
    }

    /// Run one stage to completion.
    pub fn run_stage(&self, stage: BuildStage) -> Result<(), SandboxError> {
        let mut child = self.spawn_stage(stage)?;
        let status = child.wait().map_err(SandboxError::Spawn)?;
        if !status.success() {
            return Err(SandboxError::BuildFailed { stage, status });
        }
        Ok(())
    }

    /// Run the full two-stage sequence: fetch (networked), then build
    /// (network-isolated).
    pub fn build(&self) -> Result<(), SandboxError> {
        self.run_stage(BuildStage::Fetch)?;
        self.run_stage(BuildStage::Build)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_stage_flags() {
        assert_eq!(
            BuildStage::Fetch.makepkg_args(),
            &["--nobuild", "--nodeps", "--noconfirm"]
        );
        assert!(BuildStage::Fetch.allow_network());
    }

    #[test]
    fn build_stage_flags() {
        assert_eq!(
            BuildStage::Build.makepkg_args(),
            &["--noextract", "--nodeps", "--noconfirm", "--force"]
        );
        assert!(!BuildStage::Build.allow_network());
    }

    #[test]
    fn missing_bwrap_is_reported_not_panicked() {
        // This test's assertion depends on the host; only check the happy
        // path type-checks when bwrap genuinely is missing from PATH.
        if !SandboxRunner::bwrap_available() {
            let runner = SandboxRunner::new("/tmp/does-not-matter");
            assert!(matches!(
                runner.spawn_stage(BuildStage::Fetch),
                Err(SandboxError::BwrapNotFound)
            ));
        }
    }
}
