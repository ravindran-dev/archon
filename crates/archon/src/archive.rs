//! Immutable handoff from an untrusted build directory to the root-owned
//! receiver. Native pacman verifies staged archives and adjacent signatures.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::io::{Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Validate the administrator-installed helper before starting any AUR build.
pub fn check_handoff_policy() -> Result<()> {
    crate::handoff::check_helper()
}

pub struct PackageArchive {
    source: PathBuf,
    snapshot: File,
    signature: Option<File>,
}

impl PackageArchive {
    pub fn snapshot(source: &Path) -> Result<Self> {
        // Do not follow a symlink substituted after directory enumeration;
        // O_NONBLOCK avoids hanging on a substituted FIFO before fstat.
        let snapshot = Self::freeze(source, crate::handoff::MAX_PACKAGE_BYTES)?;
        let mut signature = source.as_os_str().to_os_string();
        signature.push(".sig");
        let signature = PathBuf::from(signature);
        let signature = match std::fs::symlink_metadata(&signature) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
            Ok(_) => Some(Self::freeze(
                &signature,
                crate::handoff::MAX_SIGNATURE_BYTES,
            )?),
        };
        Ok(Self {
            source: source.into(),
            snapshot,
            signature,
        })
    }

    fn freeze(source: &Path, limit: u64) -> Result<File> {
        let input = OpenOptions::new()
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
        let copied = std::io::copy(&mut input.take(limit + 1), &mut snapshot)
            .context("failed to copy built package into immutable snapshot")?;
        anyhow::ensure!(
            copied <= limit,
            "archive/signature exceeds handoff size limit"
        );
        snapshot.seek(SeekFrom::Start(0))?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot seal package snapshot; refusing mutable archive handoff");
        }
        Ok(snapshot)
    }

    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.snapshot.as_raw_fd()
        ))
    }

    #[cfg(test)]
    pub fn from_fixture(source: &Path, signature: Option<&Path>) -> Result<Self> {
        Ok(Self {
            source: source.to_owned(),
            snapshot: Self::freeze(source, crate::handoff::MAX_PACKAGE_BYTES)?,
            signature: signature
                .map(|path| Self::freeze(path, crate::handoff::MAX_SIGNATURE_BYTES))
                .transpose()?,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Serialize immutable snapshots; the helper supplies adjacent signatures
    /// and native pacman performs the actual configured verification.
    pub fn write_transaction(archives: &[Self], writer: &mut impl Write) -> Result<()> {
        anyhow::ensure!(
            !archives.is_empty() && archives.len() <= crate::handoff::MAX_PACKAGES as usize,
            "invalid archive transaction count"
        );
        writer.write_all(crate::handoff::MAGIC)?;
        writer.write_all(&(archives.len() as u32).to_le_bytes())?;
        for archive in archives {
            let name = archive
                .source
                .file_name()
                .and_then(|name| name.to_str())
                .context("invalid package filename")?;
            anyhow::ensure!(
                crate::handoff::valid_name(name),
                "invalid package archive filename"
            );
            writer.write_all(&(name.len() as u16).to_le_bytes())?;
            writer.write_all(name.as_bytes())?;
            write_file(&archive.snapshot, writer)?;
            writer.write_all(&[u8::from(archive.signature.is_some())])?;
            if let Some(signature) = &archive.signature {
                write_file(signature, writer)?;
            }
        }
        writer.flush()?;
        Ok(())
    }
}

fn write_file(file: &File, writer: &mut impl Write) -> Result<()> {
    let size = file.metadata()?.len();
    writer.write_all(&size.to_le_bytes())?;
    let mut offset = 0;
    let mut buffer = [0; 65536];
    while offset < size {
        let count = file.read_at(&mut buffer[..(size - offset).min(65536) as usize], offset)?;
        anyhow::ensure!(count > 0, "truncated sealed package snapshot");
        writer.write_all(&buffer[..count])?;
        offset += count as u64;
    }
    Ok(())
}

pub trait ArchiveInstaller {
    fn preflight(&self) -> Result<()>;
    fn install(&self, archives: &[PackageArchive]) -> Result<()>;
}

pub struct RootInstaller;
impl ArchiveInstaller for RootInstaller {
    fn preflight(&self) -> Result<()> {
        check_handoff_policy()
    }
    fn install(&self, archives: &[PackageArchive]) -> Result<()> {
        send_to_helper(archives)
    }
}

/// Only callers that have passed the trusted-path preflight may use this.
fn send_to_helper(archives: &[PackageArchive]) -> Result<()> {
    let mut command = if unsafe { libc::geteuid() } == 0 {
        Command::new(crate::handoff::HELPER)
    } else {
        let mut command = Command::new("/usr/bin/sudo");
        command.args(["--", crate::handoff::HELPER]);
        command
    };
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot launch administrator-installed package helper")?;
    let mut stdin = child.stdin.take().context("helper stdin unavailable")?;
    let written = PackageArchive::write_transaction(archives, &mut stdin);
    drop(stdin);
    written.context("failed to transmit package transaction to helper")?;
    let status = child.wait()?;
    if !status.success() {
        bail!("package helper exited with {status}");
    }
    Ok(())
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
