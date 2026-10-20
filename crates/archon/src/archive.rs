//! Immutable handoff from an untrusted build directory to pacman.
//!
//! Keep the sealed file open until pacman exits. Both the identity query and
//! installation open this same snapshot through the parent's procfs descriptor;
//! neither operation reopens the mutable build output. No sudo FD-preservation
//! setting or writable root staging directory is needed.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Descriptor paths cannot carry the sibling signature demanded by Required.
/// Refuse before building rather than changing the user's pacman policy.
pub fn check_handoff_policy() -> Result<()> {
    let output = Command::new("pacman-conf")
        .arg("LocalFileSigLevel")
        .output()
        .context("cannot read LocalFileSigLevel with pacman-conf; refusing AUR archive handoff")?;
    if !output.status.success() {
        bail!(
            "cannot read pacman LocalFileSigLevel: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let policy = String::from_utf8(output.stdout).context("invalid LocalFileSigLevel output")?;
    if !policy
        .lines()
        .any(|line| matches!(line.trim(), "PackageOptional" | "PackageNever"))
    {
        bail!(
            "LocalFileSigLevel requires a sibling signature, which sealed descriptor handoff cannot supply; refusing AUR installation before building (pacman policy is unchanged)"
        );
    }
    Ok(())
}

pub struct PackageArchive {
    source: PathBuf,
    snapshot: File,
}

impl PackageArchive {
    pub fn snapshot(source: &Path) -> Result<Self> {
        // Do not follow a symlink substituted after directory enumeration;
        // O_NONBLOCK avoids hanging on a substituted FIFO before fstat.
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)
            .with_context(|| format!("failed to open package archive {}", source.display()))?;
        if !input.metadata()?.is_file() {
            bail!("package archive {} is not a regular file", source.display());
        }
        let fd = unsafe {
            libc::memfd_create(
                c"archon-package".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context(
                "cannot create immutable package snapshot; refusing mutable archive handoff",
            );
        }
        let mut snapshot = unsafe { File::from_raw_fd(fd) };
        std::io::copy(&mut input, &mut snapshot)
            .context("failed to copy built package into immutable snapshot")?;
        snapshot.seek(SeekFrom::Start(0))?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot seal package snapshot; refusing mutable archive handoff");
        }
        Ok(Self {
            source: source.into(),
            snapshot,
        })
    }

    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.snapshot.as_raw_fd()
        ))
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Procfs descriptor paths have no writable sibling for `<archive>.sig`.
    /// Preserve signature checks explicitly instead of silently losing them
    /// when LocalFileSigLevel is Optional. pacman-key requires a valid signature
    /// from a fully/ultimately trusted key in pacman's system keyring.
    /// Pacman's own policy is never relaxed: Required still fails closed if it
    /// requires the sibling signature at installation time.
    pub fn verify_detached_signature(&self) -> Result<()> {
        let mut signature = self.source.as_os_str().to_os_string();
        signature.push(".sig");
        let signature = PathBuf::from(signature);
        match std::fs::symlink_metadata(&signature) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("failed to inspect package signature"),
            Ok(_) => {}
        }
        let signature = Self::snapshot(&signature)?;
        let status = Command::new("pacman-key")
            .args(["--verify", "--"])
            .arg(signature.path())
            .arg(self.path())
            .status()
            .context(
                "cannot verify detached package signature with pacman-key; refusing installation",
            )?;
        if !status.success() {
            bail!(
                "detached signature verification failed for {}; refusing installation",
                self.source.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn snapshot_survives_source_replacement_and_cannot_be_modified() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.pkg.tar.zst");
        std::fs::write(&source, b"validated bytes").unwrap();
        let archive = PackageArchive::snapshot(&source).unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, b"attacker replacement").unwrap();
        assert_eq!(std::fs::read(archive.path()).unwrap(), b"validated bytes");
        let mut writable = OpenOptions::new().write(true).open(archive.path()).unwrap();
        assert!(writable.write_all(b"changed").is_err());
        assert!(writable.set_len(0).is_err());
        assert!(writable.set_len(100).is_err());
        assert_eq!(std::fs::read(archive.path()).unwrap(), b"validated bytes");
    }

    #[test]
    fn symlink_and_fifo_archives_are_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"data").unwrap();
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(PackageArchive::snapshot(&link).is_err());
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(PackageArchive::snapshot(&fifo).is_err());
    }
}
