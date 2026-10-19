//! Reader for pacman's local install database
//! (`/var/lib/pacman/local/<pkg>-<ver>/desc`) — plain directories, not
//! tarballs, but the same `desc` format as sync databases, so we reuse
//! that parser.
//!
//! This exists purely so the CLI can tell you what a plan will *actually*
//! change: without it, every install shows its full theoretical
//! dependency closure regardless of what's already on the system, which
//! for a library-heavy package can be hundreds of already-satisfied
//! entries drowning out the handful that matter. It is never used for
//! resolution itself — pacman's own distinction between "what's
//! available" (sync dbs) and "what's installed" (local db) stays intact;
//! this is read-only, display-only information.

use std::collections::HashMap;
use std::path::Path;

use archon_types::Version;

use crate::{cache, desc};

#[derive(Debug, thiserror::Error)]
pub enum LocalDbError {
    #[error("failed to read local db {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

pub struct LocalDb {
    versions: HashMap<String, Version>,
}

impl LocalDb {
    /// Loads every `<pkg>-<ver>/desc` under `local_dir` (normally
    /// `/var/lib/pacman/local`). An individual entry that fails to parse
    /// is skipped, not fatal — this is a best-effort display aid, and the
    /// safe fallback for "couldn't tell if it's installed" is to show the
    /// package rather than hide it.
    pub fn load(local_dir: &Path) -> Result<Self, LocalDbError> {
        let entries = std::fs::read_dir(local_dir).map_err(|source| LocalDbError::Io {
            path: local_dir.display().to_string(),
            source,
        })?;
        let mut versions = HashMap::new();
        for entry in entries.flatten() {
            let desc_path = entry.path().join("desc");
            let Ok(contents) = std::fs::read_to_string(&desc_path) else { continue };
            let Ok(meta) = desc::parse("local", &contents) else { continue };
            versions.insert(meta.name, meta.version);
        }
        Ok(Self { versions })
    }

    /// Same result as [`LocalDb::load`], but backed by a disk cache at
    /// `cache_path`, invalidated by `local_dir`'s own mtime (installing,
    /// removing, or upgrading anything always touches an immediate child
    /// of this directory, which always bumps it). Measured slower than
    /// parsing the *entire* sync database despite ~15x fewer packages —
    /// dominated by per-file open/read/close syscall overhead across
    /// ~1,000 separate `desc` files, which a warm cache skips entirely.
    /// Any cache problem falls back to [`LocalDb::load`]; see the `cache`
    /// module doc comment.
    pub fn load_cached(local_dir: &Path, cache_path: &Path) -> Result<Self, LocalDbError> {
        let Ok(fingerprint) = cache::Fingerprint::of_dir(local_dir) else {
            return Self::load(local_dir);
        };
        if let Some(versions) = cache::try_read::<HashMap<String, Version>>(cache_path, &fingerprint) {
            return Ok(Self { versions });
        }
        let this = Self::load(local_dir)?;
        cache::write(cache_path, fingerprint, &this.versions);
        Ok(this)
    }

    pub fn version_of(&self, name: &str) -> Option<&Version> {
        self.versions.get(name)
    }

    /// Whether `name` is installed at `min_version` or newer — the same
    /// bar `pacman -S --needed` uses to decide whether to skip a package.
    pub fn satisfies(&self, name: &str, min_version: &Version) -> bool {
        self.versions
            .get(name)
            .is_some_and(|installed| installed.vercmp(min_version).is_ok_and(|order| order != std::cmp::Ordering::Less))
    }

    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// All installed package names — for handing the resolver a
    /// preference hint (see `Resolver::prefer_installed`), not for
    /// resolution itself.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.versions.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(entries: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, version) in entries {
            let pkg_dir = dir.path().join(format!("{name}-{version}"));
            std::fs::create_dir_all(&pkg_dir).unwrap();
            std::fs::write(
                pkg_dir.join("desc"),
                format!("%NAME%\n{name}\n\n%VERSION%\n{version}\n\n%BASE%\n{name}\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn reads_installed_versions() {
        let dir = fixture(&[("glibc", "2.43-1"), ("bash", "5.3.15-1")]);
        let db = LocalDb::load(dir.path()).unwrap();
        assert_eq!(db.len(), 2);
        assert_eq!(db.version_of("glibc").unwrap().to_string(), "2.43-1");
    }

    #[test]
    fn satisfies_checks_version_floor() {
        let dir = fixture(&[("foo", "2.0-1")]);
        let db = LocalDb::load(dir.path()).unwrap();
        assert!(db.satisfies("foo", &Version::parse("1.0-1").unwrap()));
        assert!(db.satisfies("foo", &Version::parse("2.0-1").unwrap()));
        assert!(!db.satisfies("foo", &Version::parse("3.0-1").unwrap()));
        assert!(!db.satisfies("not-installed", &Version::parse("1.0-1").unwrap()));
    }

    #[test]
    fn skips_unparseable_entries_without_failing() {
        let dir = fixture(&[("good", "1.0-1")]);
        std::fs::create_dir_all(dir.path().join("garbage-dir")).unwrap();
        std::fs::write(dir.path().join("garbage-dir").join("desc"), "not a valid desc file").unwrap();
        let db = LocalDb::load(dir.path()).unwrap();
        assert_eq!(db.len(), 1);
        assert!(db.version_of("good").is_some());
    }
}
