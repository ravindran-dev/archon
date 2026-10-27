//! Private, durable workflow journal. This never edits a native package database.
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use archon_distro::transaction::TransactionPlan;
use serde::{Deserialize, Serialize};

const MAX_RECORD: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Planned,
    Resolving,
    AwaitingConfirmation,
    Downloading,
    Verifying,
    Committing,
    Completed,
    Cancelled,
    Failed,
    Uncertain,
}

impl Status {
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            Self::Resolving
                | Self::AwaitingConfirmation
                | Self::Downloading
                | Self::Verifying
                | Self::Committing
        )
    }

    pub fn can_apply(self) -> bool {
        matches!(self, Self::Planned | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub start_ticks: String,
}

impl ProcessIdentity {
    pub fn read(pid: u32) -> Result<Option<Self>> {
        ensure!(
            pid > 0 && pid <= i32::MAX as u32,
            "invalid helper process id"
        );
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let (_, fields) = stat.rsplit_once(") ").context("invalid process identity")?;
        let fields: Vec<_> = fields.split_whitespace().collect();
        if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
            return Ok(None);
        }
        Ok(Some(Self {
            pid,
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .into(),
            start_ticks: fields
                .get(19)
                .context("missing process start time")?
                .to_string(),
        }))
    }

    pub fn current() -> Result<Self> {
        Self::read(std::process::id())?.context("cannot identify current process")
    }

    pub fn alive(&self) -> Result<bool> {
        Ok(Self::read(self.pid)?.as_ref() == Some(self))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    pub id: String,
    pub status: Status,
    pub created_at: u64,
    pub updated_at: u64,
    pub plan: TransactionPlan,
    pub owner: ProcessIdentity,
    pub helper: Option<ProcessIdentity>,
    pub error: Option<String>,
}

impl Record {
    pub fn new(plan: TransactionPlan) -> Result<Self> {
        plan.validate().map_err(anyhow::Error::msg)?;
        Ok(Self {
            schema_version: 1,
            id: std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
                .trim()
                .into(),
            status: Status::Planned,
            created_at: now(),
            updated_at: now(),
            plan,
            owner: ProcessIdentity::current()?,
            helper: None,
            error: None,
        })
    }

    pub fn set_status(&mut self, status: Status) {
        self.status = status;
        self.updated_at = now();
    }

    pub fn recover(&mut self) -> Result<()> {
        if !self.status.in_flight() {
            return Ok(());
        }
        ensure!(
            !self.owner.alive()?,
            "transaction coordinator is still running; inspect it before recovery"
        );
        if let Some(helper) = &self.helper {
            ensure!(
                !helper.alive()?,
                "native transaction helper is still running; do not retry or remove native locks"
            );
        }
        if self.status == Status::Committing {
            self.set_status(Status::Uncertain);
            self.error = Some("Coordinator interrupted during native commit; package/script state requires native inspection".into());
        } else {
            self.set_status(Status::Failed);
            self.error =
                Some("Coordinator interrupted before the durable commit checkpoint".into());
        }
        Ok(())
    }
}

pub fn state_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("set HOME or XDG_STATE_HOME to store transaction recovery records")?;
    ensure!(
        base.is_absolute(),
        "transaction state directory must be absolute"
    );
    Ok(base.join("archon/transactions"))
}

pub struct Store {
    directory: File,
    _lock: Option<File>,
}

impl Store {
    #[cfg(test)]
    pub(crate) fn for_test(path: &Path) -> Result<Self> {
        // Tests start from an already-held private directory, independent of
        // the execution sandbox's remapped ownership of filesystem ancestors.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Self::from_directory(File::open(path)?, true)
    }
    pub fn open(path: &Path, exclusive: bool) -> Result<Self> {
        let directory = secure_directory(path)?;
        Self::from_directory(directory, exclusive)
    }

    fn from_directory(directory: File, exclusive: bool) -> Result<Self> {
        let meta = directory.metadata()?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
            "unsafe transaction state directory"
        );
        let mut store = Self {
            directory,
            _lock: None,
        };
        if exclusive {
            let lock = store.open_file(".lock", libc::O_RDWR | libc::O_CREAT)?;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                bail!("transaction journal is busy; another Archon workflow may be active");
            }
            store._lock = Some(lock);
        }
        Ok(store)
    }

    fn directory_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
    }

    fn open_file(&self, name: &str, flags: i32) -> Result<File> {
        let name = CString::new(name)?;
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot open private transaction state");
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0
                && meta.nlink() == 1,
            "unsafe transaction journal file"
        );
        Ok(file)
    }

    pub fn save(&self, record: &Record) -> Result<()> {
        ensure!(
            self._lock.is_some(),
            "journal writes require exclusive workflow lock"
        );
        validate_id(&record.id)?;
        ensure!(record.schema_version == 1, "unsupported journal schema");
        record.plan.validate().map_err(anyhow::Error::msg)?;
        let bytes = serde_json::to_vec_pretty(record)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECORD,
            "transaction record too large"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(self.directory_path())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.directory_path().join(format!("{}.json", record.id)))?;
        self.directory.sync_all()?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Record> {
        validate_id(id)?;
        let file = self.open_file(&format!("{id}.json"), libc::O_RDONLY)?;
        let mut bytes = Vec::new();
        file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECORD,
            "transaction record too large"
        );
        let record: Record = serde_json::from_slice(&bytes)
            .context("corrupt transaction journal; native state must be inspected")?;
        ensure!(
            record.schema_version == 1 && record.id == id,
            "transaction journal identity/schema mismatch"
        );
        record.plan.validate().map_err(anyhow::Error::msg)?;
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(self.directory_path())? {
            let entry = entry?;
            if let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            {
                records.push(self.load(name)?);
            }
        }
        records.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        Ok(records)
    }

    pub fn log(&self, id: &str) -> Result<File> {
        validate_id(id)?;
        self.open_file(
            &format!("{id}.log"),
            libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT,
        )
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "invalid transaction id"
    );
    Ok(())
}

fn secure_directory(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "state directory must be absolute");
    let mut directory = File::open("/")?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            ensure!(
                component == Component::RootDir,
                "state path cannot contain parent traversal"
            );
            continue;
        };
        let meta = directory.metadata()?;
        let uid = unsafe { libc::geteuid() };
        let trusted_tmp = meta.uid() == 0 && meta.mode() & libc::S_ISVTX != 0;
        ensure!(
            (meta.uid() == 0 || meta.uid() == uid) && (meta.mode() & 0o022 == 0 || trusted_tmp),
            "unsafe transaction state parent"
        );
        let name = CString::new(name.as_encoded_bytes())?;
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("state path must contain real directories without symlinks");
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let meta = directory.metadata()?;
    ensure!(
        meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "transaction state directory must be owned by this user and private (0700)"
    );
    Ok(directory)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn record() -> Record {
        Record::new(
            serde_json::from_str(include_str!("../tests/fixtures/native-plan.json")).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn durable_records_roundtrip_and_corruption_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::for_test(dir.path()).unwrap();
        let mut record = record();
        store.save(&record).unwrap();
        record.set_status(Status::Committing);
        store.save(&record).unwrap();
        assert_eq!(store.load(&record.id).unwrap().status, Status::Committing);
        assert_eq!(store.list().unwrap().len(), 1);
        std::fs::write(
            dir.path().join(format!("{}.json", record.id)),
            b"{truncated",
        )
        .unwrap();
        assert!(store.load(&record.id).is_err());
        assert!(store.list().is_err());
    }

    #[test]
    fn interruption_after_commit_is_uncertain_and_cannot_be_replayed() {
        let mut record = record();
        record.owner.boot_id = "previous-boot".into();
        record.set_status(Status::Committing);
        record.recover().unwrap();
        assert_eq!(record.status, Status::Uncertain);
        assert!(!record.status.can_apply());
        record.set_status(Status::Completed);
        assert!(!record.status.can_apply());
    }

    #[test]
    fn precommit_interruption_can_retry_but_a_live_helper_blocks_recovery() {
        let mut record = record();
        record.owner.boot_id = "previous-boot".into();
        record.set_status(Status::Downloading);
        record.helper = Some(ProcessIdentity::current().unwrap());
        assert!(record.recover().is_err());
        record.helper = None;
        record.recover().unwrap();
        assert_eq!(record.status, Status::Failed);
        assert!(record.status.can_apply());
    }

    #[test]
    fn hardlinked_state_is_rejected_without_modifying_its_other_link() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::for_test(dir.path()).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        std::fs::hard_link(&outside, dir.path().join("bad.json")).unwrap();
        assert!(store.load("bad").is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }

    #[test]
    fn journal_lock_is_exclusive_and_released_by_process_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::for_test(dir.path()).unwrap();
        assert!(Store::from_directory(File::open(dir.path()).unwrap(), true).is_err());
        assert!(Store::from_directory(File::open(dir.path()).unwrap(), false).is_ok());
        drop(first);
        assert!(Store::from_directory(File::open(dir.path()).unwrap(), true).is_ok());
    }

    #[test]
    fn symlink_parents_and_nonregular_records_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(Store::open(&link.join("child"), true).is_err());
        assert!(!real.join("child").exists());
        let store = Store::for_test(dir.path()).unwrap();
        symlink("/etc/passwd", dir.path().join("bad.json")).unwrap();
        assert!(store.load("bad").is_err());
        assert!(store.load("../bad").is_err());
    }

    #[test]
    fn process_identity_includes_boot_and_start_time_to_prevent_pid_reuse() {
        let mut identity = ProcessIdentity::current().unwrap();
        assert!(identity.alive().unwrap());
        identity.start_ticks.push('0');
        assert!(!identity.alive().unwrap());
    }
}
