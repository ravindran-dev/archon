//! Advisory locks on directory descriptors: no stale lockfiles to remove.
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};

#[derive(Debug)]
pub struct BuildLock(File);

impl BuildLock {
    pub fn acquire(directory: &Path) -> Result<Self> {
        let file = File::open(directory)
            .with_context(|| format!("cannot open build directory {}", directory.display()))?;
        if !file.metadata()?.is_dir() {
            bail!("build path is not a directory: {}", directory.display());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                bail!(
                    "another Archon process is using {}; wait for it to finish and retry",
                    directory.display()
                );
            }
            return Err(error).context("cannot lock build directory");
        }
        Ok(Self(file))
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_builds_are_refused_and_lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = BuildLock::acquire(dir.path()).unwrap();
        assert!(
            BuildLock::acquire(dir.path())
                .unwrap_err()
                .to_string()
                .contains("another Archon process")
        );
        drop(first);
        assert!(BuildLock::acquire(dir.path()).is_ok());
    }
}
