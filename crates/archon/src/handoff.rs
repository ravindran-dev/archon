//! Bounded package-data protocol and private installation staging.
//! No wire field supplies a command, option, staging directory or config path.
#![allow(dead_code)] // The CLI and standalone privileged receiver use different subsets.

use std::collections::HashSet;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};

pub const HELPER: &str = "/usr/local/libexec/archon-install-helper";
pub const MAGIC: &[u8; 8] = b"ARCHON1\n";
pub const MAX_PACKAGES: u32 = 128;
pub const MAX_PACKAGE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: u64 = 1024 * 1024;
const MAX_TRANSACTION_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const DISK_RESERVE: u64 = 256 * 1024 * 1024;
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && name.contains(".pkg.tar")
        && !name.ends_with(".sig")
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"@._+:-".contains(&c))
}

/// Every parent must be administrator-controlled, including the executable.
pub fn check_helper() -> Result<()> {
    let path = Path::new(HELPER);
    let mut component = PathBuf::from("/");
    let parts: Vec<_> = path.components().skip(1).collect();
    for (index, part) in parts.iter().enumerate() {
        component.push(part);
        let metadata = std::fs::symlink_metadata(&component).with_context(|| {
            format!("installation helper is not securely installed at {HELPER}; run `archon setup-helper` after building the companion helper")
        })?;
        ensure!(
            !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.mode() & 0o022 == 0,
            "unsafe helper path {}: must be root-owned, without symlinks or group/other writes",
            component.display()
        );
        if index + 1 == parts.len() {
            ensure!(
                metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.mode() & 0o6000 == 0,
                "helper must be an ordinary executable, never setuid/setgid"
            );
        } else {
            ensure!(metadata.is_dir(), "helper parent is not a directory");
        }
    }
    Ok(())
}

pub fn prepare_root() -> Result<PathBuf> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "installation helper requires administrator privileges"
    );
    for (path, mode) in [
        ("/var", 0o755),
        ("/var/lib", 0o755),
        ("/var/lib/archon", 0o700),
        ("/var/lib/archon/transactions", 0o700),
    ] {
        let path = Path::new(path);
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DirBuilder::new().mode(mode).create(path)?;
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.mode() & 0o022 == 0
                && ((path == Path::new("/var/lib/archon/transactions")
                    || path == Path::new("/var/lib/archon"))
                    || metadata.mode() & 0o777 == mode),
            "unsafe installation staging parent {}",
            path.display()
        );
    }
    Ok(PathBuf::from("/var/lib/archon/transactions"))
}

/// Install an already-built helper with the system's trusted `install` tool.
/// This only copies bytes; it never executes the candidate helper as root.
pub fn install_helper(source: &Path) -> Result<()> {
    let source = source
        .canonicalize()
        .context("cannot locate helper binary")?;
    let file = File::open(&source).context("cannot read helper binary")?;
    ensure!(
        file.metadata()?.is_file(),
        "helper source is not a regular file"
    );
    let status = std::process::Command::new("/usr/bin/sudo")
        .args([
            "--",
            "/usr/bin/install",
            "-D",
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "0755",
        ])
        .arg(&source)
        .arg(HELPER)
        .status()
        .context("cannot run administrator setup; install helper manually as documented")?;
    ensure!(
        status.success(),
        "administrator helper installation failed with {status}"
    );
    check_helper()
}

pub struct StagedTransaction {
    directory: PathBuf,
    packages: Vec<PathBuf>,
}

impl StagedTransaction {
    /// `base` has already been validated by prepare_root in the executable.
    /// Unit tests use their own private, unprivileged temporary base.
    pub fn receive(base: &Path, reader: &mut impl Read) -> Result<Self> {
        let mut magic = [0; 8];
        read_exact(reader, &mut magic)?;
        ensure!(
            &magic == MAGIC,
            "unsupported installation protocol; update both Archon and its helper"
        );
        let count = u32::from_le_bytes(read_array(reader)?);
        ensure!((1..=MAX_PACKAGES).contains(&count), "invalid package count");
        let base_metadata = std::fs::symlink_metadata(base)?;
        ensure!(
            base_metadata.is_dir()
                && !base_metadata.file_type().is_symlink()
                && base_metadata.uid() == unsafe { libc::geteuid() }
                && (unsafe { libc::geteuid() } != 0 || base_metadata.mode() & 0o077 == 0),
            "staging directory must be private and owned by the effective user"
        );
        let mut template = base
            .join("transaction.XXXXXX")
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        template.push(0);
        let created = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        if created.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let directory = PathBuf::from(std::ffi::CStr::from_bytes_with_nul(&template)?.to_str()?);
        let directory_metadata = std::fs::symlink_metadata(&directory)?;
        ensure!(
            directory_metadata.is_dir()
                && !directory_metadata.file_type().is_symlink()
                && directory_metadata.uid() == unsafe { libc::geteuid() }
                && directory_metadata.mode() & 0o777 == 0o700,
            "created transaction directory is not private"
        );
        let mut transaction = Self {
            directory,
            packages: Vec::new(),
        };
        let mut names = HashSet::new();
        let mut total = 0u64;
        for _ in 0..count {
            let length = u16::from_le_bytes(read_array(reader)?) as usize;
            ensure!(
                (1..=255).contains(&length),
                "invalid package filename length"
            );
            let mut name = vec![0; length];
            read_exact(reader, &mut name)?;
            let name = String::from_utf8(name)?;
            ensure!(
                valid_name(&name) && names.insert(name.clone()),
                "invalid or duplicate package filename"
            );
            let path = transaction.directory.join(&name);
            let size = u64::from_le_bytes(read_array(reader)?);
            ensure!(
                size > 0 && size <= MAX_PACKAGE_BYTES,
                "invalid package size"
            );
            copy_payload(reader, &path, size, &mut total)?;
            let present = read_array::<1>(reader)?[0];
            ensure!(present <= 1, "invalid signature presence marker");
            if present == 1 {
                let size = u64::from_le_bytes(read_array(reader)?);
                ensure!(
                    (1..=MAX_SIGNATURE_BYTES).contains(&size),
                    "invalid signature size"
                );
                let signature = transaction.directory.join(format!("{name}.sig"));
                copy_payload(reader, &signature, size, &mut total)?;
            }
            transaction.packages.push(path);
        }
        ensure!(
            !INTERRUPTED.load(Ordering::Relaxed),
            "installation interrupted"
        );
        let mut extra = [0; 1];
        ensure!(
            reader.read(&mut extra)? == 0,
            "trailing installation protocol data"
        );
        Ok(transaction)
    }

    pub fn packages(&self) -> &[PathBuf] {
        &self.packages
    }
}

#[cfg(test)]
pub fn audit_private_tempdir() -> Result<PathBuf> {
    let mut template = std::env::temp_dir()
        .join("archon-audit-XXXXXX")
        .into_os_string()
        .as_encoded_bytes()
        .to_vec();
    template.push(0);
    let created = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    ensure!(!created.is_null(), "cannot create audit temp directory");
    Ok(PathBuf::from(
        std::ffi::CStr::from_bytes_with_nul(&template)?.to_str()?,
    ))
}

impl Drop for StagedTransaction {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.directory) {
            eprintln!(
                "warning: cannot remove private transaction {}: {error}",
                self.directory.display()
            );
        }
    }
}

fn copy_payload(reader: &mut impl Read, path: &Path, size: u64, total: &mut u64) -> Result<()> {
    *total = total
        .checked_add(size)
        .context("transaction size overflow")?;
    ensure!(
        *total <= MAX_TRANSACTION_BYTES,
        "transaction exceeds size limit"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut space = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(file.as_raw_fd(), space.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let space = unsafe { space.assume_init() };
    let available = u128::from(space.f_bavail) * u128::from(space.f_frsize);
    ensure!(
        available >= u128::from(size) + u128::from(DISK_RESERVE),
        "insufficient staging space (256 MiB reserve required)"
    );
    let mut remaining = size;
    let mut buffer = [0; 65536];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        read_exact(reader, &mut buffer[..count])?;
        file.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    file.sync_all()?;
    Ok(())
}

fn read_exact(reader: &mut impl Read, mut bytes: &mut [u8]) -> Result<()> {
    while !bytes.is_empty() {
        ensure!(
            !INTERRUPTED.load(Ordering::Relaxed),
            "installation interrupted"
        );
        match reader.read(bytes) {
            Ok(0) => bail!("truncated installation protocol"),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    read_exact(reader, &mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(name: &str, signature: bool) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(7u64.to_le_bytes());
        bytes.extend(b"package");
        bytes.push(u8::from(signature));
        if signature {
            bytes.extend(9u64.to_le_bytes());
            bytes.extend(b"signature");
        }
        bytes
    }
    #[test]
    fn signed_transaction_is_private_and_keeps_adjacent_signature() {
        let root = tempfile::tempdir().unwrap();
        let stage = StagedTransaction::receive(
            root.path(),
            &mut message("fixture-1-1-any.pkg.tar.zst", true).as_slice(),
        )
        .unwrap();
        let path = &stage.packages()[0];
        assert_eq!(std::fs::read(path).unwrap(), b"package");
        assert_eq!(
            std::fs::read(path.with_file_name("fixture-1-1-any.pkg.tar.zst.sig")).unwrap(),
            b"signature"
        );
        assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        drop(stage);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn traversal_options_and_signature_names_are_rejected_and_cleaned() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "../escape.pkg.tar",
            "/escape.pkg.tar",
            "--root.pkg.tar",
            "file.pkg.tar.sig",
            "bad\n.pkg.tar",
        ] {
            assert!(
                StagedTransaction::receive(root.path(), &mut message(name, false).as_slice())
                    .is_err()
            );
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }
    #[test]
    fn truncated_or_trailing_data_never_leaves_staging() {
        let root = tempfile::tempdir().unwrap();
        let bytes = message("fixture.pkg.tar", true);
        for end in 0..bytes.len() {
            assert!(StagedTransaction::receive(root.path(), &mut &bytes[..end]).is_err());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(StagedTransaction::receive(root.path(), &mut trailing.as_slice()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn duplicate_archives_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = message("fixture.pkg.tar", false);
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&message("fixture.pkg.tar", false)[12..]);
        assert!(StagedTransaction::receive(root.path(), &mut bytes.as_slice()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn zero_sized_signature_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = message("fixture.pkg.tar", true);
        let signature_size_offset = bytes.len() - 9 - 8;
        bytes[signature_size_offset..signature_size_offset + 8]
            .copy_from_slice(&0u64.to_le_bytes());
        assert!(StagedTransaction::receive(root.path(), &mut bytes.as_slice()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
