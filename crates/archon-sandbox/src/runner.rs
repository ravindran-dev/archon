use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
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
    #[error("refusing to run a build or key import as root; select a non-root build user")]
    RootBuildDenied,
    #[error("failed to launch bwrap: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("{stage:?} stage failed: makepkg exited with {status}")]
    BuildFailed {
        stage: BuildStage,
        status: ExitStatus,
    },
}

/// Builds the `cgroup.procs` path as a `CString` up front — `pre_exec`
/// can't allocate, so this must happen before `spawn()`, not inside the
/// closure.
fn cgroup_procs_cstring(cgroup_dir: &Path) -> CString {
    let path = cgroup_dir.join("cgroup.procs");
    CString::new(path.as_os_str().as_bytes()).expect("cgroup path must not contain a NUL byte")
}

/// Writes the calling (post-fork, pre-exec) process's own pid into
/// `cgroup_procs_path`. Raw syscalls only, no allocation — this runs
/// inside a `pre_exec` closure where only async-signal-safe operations
/// are permitted.
fn join_cgroup_raw(cgroup_procs_path: &CString) -> std::io::Result<()> {
    let fd = unsafe { libc::open(cgroup_procs_path.as_ptr(), libc::O_WRONLY) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let pid = unsafe { libc::getpid() };
    let mut buf = [0u8; 12]; // i32::MAX is 10 digits; comfortably fits
    let digits = write_decimal(&mut buf, pid);
    let ret = unsafe { libc::write(fd, digits.as_ptr().cast(), digits.len()) };
    let write_err = if ret < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe { libc::close(fd) };
    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Formats `n` as decimal ASCII into `buf`, returning the written slice.
/// No allocation — `itoa`-style manual digit extraction, since pre_exec
/// forbids the heap allocation a normal `n.to_string()` would need.
fn write_decimal(buf: &mut [u8; 12], mut n: i32) -> &[u8] {
    if n == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}

/// Drives one AUR package's build through bubblewrap.
pub struct SandboxRunner {
    pkg_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    run_as: Option<(u32, u32)>,
    join_cgroup: Option<PathBuf>,
    capture_output: bool,
}

impl SandboxRunner {
    pub fn new(pkg_dir: impl Into<PathBuf>) -> Self {
        Self {
            pkg_dir: pkg_dir.into(),
            extra_env: Vec::new(),
            run_as: None,
            join_cgroup: None,
            capture_output: false,
        }
    }

    /// Pipes the spawned process's stdout/stderr instead of inheriting the
    /// parent's, so a caller can show a spinner in their place and only
    /// print the real output if the build fails. Off by default — plain
    /// inherited stdio is what the sandbox example and manual testing want.
    pub fn capture_output(mut self, yes: bool) -> Self {
        self.capture_output = yes;
        self
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

    /// Moves the sandboxed process into `cgroup_dir` (a cgroup v2
    /// directory) before `bwrap` execs, so an eBPF monitor scoped to that
    /// cgroup sees everything from the very first instruction onward.
    ///
    /// This must happen inside the child's own `pre_exec`, not via a
    /// `cgroup.procs` write from the parent after `spawn()` returns:
    /// `bwrap --unshare-pid` forks internally to create the new PID
    /// namespace's init process, and that fork can — and, observed on live
    /// testing, reliably does — happen before a *separate* process (ours)
    /// completes an unrelated `write()` syscall. Whichever process actually
    /// does the build's work then inherits whatever cgroup was current at
    /// that fork, which without this fix was wherever the shell that ran
    /// `archon` happened to be (e.g. the terminal's own systemd scope) —
    /// not the cgroup the monitor was told to watch. Joining from inside
    /// `pre_exec`, before `bwrap` itself has run a single instruction,
    /// makes that race impossible: every fork bwrap performs afterward
    /// inherits the correct cgroup by construction.
    pub fn join_cgroup(mut self, cgroup_dir: impl Into<PathBuf>) -> Self {
        self.join_cgroup = Some(cgroup_dir.into());
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
        let mut command = vec!["makepkg".into()];
        command.extend(stage.makepkg_args().iter().map(|s| s.to_string()));
        self.spawn_command(stage, &command)
    }

    /// Run preparation commands behind the same filesystem and privilege
    /// boundary as makepkg. In particular, never import package keys on the host.
    pub fn spawn_command(
        &self,
        stage: BuildStage,
        command: &[String],
    ) -> Result<Child, SandboxError> {
        if self.run_as.is_some_and(|(uid, _)| uid == 0)
            || (unsafe { libc::geteuid() } == 0 && self.run_as.is_none())
        {
            return Err(SandboxError::RootBuildDenied);
        }
        if !Self::bwrap_available() {
            return Err(SandboxError::BwrapNotFound);
        }
        let policy = SandboxPolicy {
            pkg_dir: self.pkg_dir.clone(),
            allow_network: stage.allow_network(),
            extra_env: self.extra_env.clone(),
        };
        let args = policy.bwrap_args(command);
        let mut cmd = Command::new("bwrap");
        cmd.args(&args);
        if self.capture_output {
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
        }
        if self.join_cgroup.is_some() || self.run_as.is_some() {
            // Precompute everything the closure needs *before* fork: a
            // pre_exec closure runs in the forked child before exec, where
            // only async-signal-safe calls are permitted (raw syscalls, no
            // heap allocation) — so the CString must already exist, just
            // captured by the closure, not built inside it.
            let cgroup_procs_path = self
                .join_cgroup
                .as_ref()
                .map(|dir| cgroup_procs_cstring(dir));
            let run_as = self.run_as;
            unsafe {
                cmd.pre_exec(move || {
                    // Join the cgroup first, while still root if we are —
                    // must happen before bwrap runs (see join_cgroup's
                    // doc comment for the race this closes), and before
                    // any privilege drop below (an unprivileged uid may
                    // lack permission to write another cgroup's procs).
                    if let Some(path) = &cgroup_procs_path {
                        join_cgroup_raw(path)?;
                    }
                    // `Command::uid`/`gid` alone would leave root's
                    // supplementary groups attached to the child;
                    // `Command::groups` (which would clear them) is still
                    // unstable, so the whole drop is done here instead, in
                    // the only safe order: groups, then gid, then uid —
                    // dropping uid first would leave us without permission
                    // to change the others.
                    if let Some((uid, gid)) = run_as {
                        if libc::setgroups(1, &gid as *const u32) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::setgid(gid) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::setuid(uid) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
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
    fn write_decimal_formats_correctly() {
        let mut buf = [0u8; 12];
        assert_eq!(write_decimal(&mut buf, 0), b"0");
        assert_eq!(write_decimal(&mut buf, 7), b"7");
        assert_eq!(write_decimal(&mut buf, 42), b"42");
        assert_eq!(write_decimal(&mut buf, 123456), b"123456");
        assert_eq!(write_decimal(&mut buf, i32::MAX), b"2147483647");
    }

    #[test]
    fn cgroup_procs_cstring_appends_filename() {
        let cstring = cgroup_procs_cstring(Path::new("/sys/fs/cgroup/archon/build-foo"));
        assert_eq!(
            cstring.to_str().unwrap(),
            "/sys/fs/cgroup/archon/build-foo/cgroup.procs"
        );
    }

    #[test]
    fn root_build_identity_is_rejected_before_any_spawn() {
        let runner = SandboxRunner::new("/tmp/unused").run_as(0, 0);
        assert!(matches!(
            runner.spawn_command(BuildStage::Fetch, &["gpg".into()]),
            Err(SandboxError::RootBuildDenied)
        ));
    }

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
