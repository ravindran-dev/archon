//! Reader for pacman sync databases.
//!
//! A sync db (`/var/lib/pacman/sync/core.db`, `extra.db`, ...) is a
//! compressed tarball containing one directory per package, each with a
//! `desc` file in a simple `%FIELD%\nvalue\n...` format. We read those —
//! and only those — directly. We never write pacman's databases; installs
//! are always delegated to pacman itself.
//!
//! Reading is read-only and needs no privileges, so this crate is safe to
//! run on a live system.

mod desc;
mod local_db;
mod sync_db;

pub use desc::DescParseError;
pub use local_db::{LocalDb, LocalDbError};
pub use sync_db::{LoadError, SyncDb, SyncDbError};

use std::path::Path;

use archon_types::{PackageMeta, PackageProvider, ProviderError};

/// Default repo precedence, mirroring a stock pacman.conf. Repos found on
/// disk but not listed here sort after these, alphabetically.
pub const DEFAULT_REPO_ORDER: &[&str] = &["core", "extra", "multilib"];

/// All configured official repos, queried in pacman.conf order so that
/// e.g. `core` shadows `extra` the same way pacman would pick.
pub struct OfficialRepos {
    dbs: Vec<SyncDb>,
}

impl OfficialRepos {
    /// Load every `*.db` under `sync_dir` (normally
    /// `/var/lib/pacman/sync`). `repo_order` fixes precedence; unknown
    /// repos in the directory are appended after the known ones,
    /// alphabetically. Stray files (mirror download temp files, `.files`
    /// databases) are ignored.
    pub fn load(sync_dir: &Path, repo_order: &[&str]) -> Result<Self, SyncDbError> {
        let io_err = |source| SyncDbError::Io {
            path: sync_dir.display().to_string(),
            source,
        };
        let mut paths: Vec<_> = std::fs::read_dir(sync_dir)
            .map_err(io_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_err)?
            .into_iter()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "db") && p.is_file())
            .collect();

        let rank = |p: &Path| -> (usize, String) {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let idx = repo_order
                .iter()
                .position(|r| *r == stem)
                .unwrap_or(repo_order.len());
            (idx, stem)
        };
        paths.sort_by_key(|p| rank(p));

        let dbs = paths
            .iter()
            .map(|p| SyncDb::load(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { dbs })
    }

    pub fn from_dbs(dbs: Vec<SyncDb>) -> Self {
        Self { dbs }
    }

    pub fn repo_names(&self) -> impl Iterator<Item = &str> {
        self.dbs.iter().map(|db| db.name.as_str())
    }

    pub fn package_count(&self) -> usize {
        self.dbs.iter().map(|db| db.len()).sum()
    }
}

impl PackageProvider for OfficialRepos {
    fn label(&self) -> &str {
        "official"
    }

    fn get(&self, names: &[String]) -> Result<Vec<PackageMeta>, ProviderError> {
        Ok(names
            .iter()
            .filter_map(|name| {
                // First repo in precedence order wins, like pacman.
                self.dbs.iter().find_map(|db| db.get(name)).cloned()
            })
            .collect())
    }

    fn find_providers(&self, dep_name: &str) -> Result<Vec<PackageMeta>, ProviderError> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for db in &self.dbs {
            for pkg in db.providers_of(dep_name) {
                if seen.insert(pkg.name.clone()) {
                    out.push(pkg.clone());
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_from(entries: &[(&str, &str, &str)], repo: &str) -> SyncDb {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, version, extra) in entries {
            let desc = format!("%NAME%\n{name}\n\n%VERSION%\n{version}\n\n{extra}");
            let mut header = tar::Header::new_ustar();
            header.set_size(desc.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{name}-{version}/desc"), desc.as_bytes())
                .unwrap();
        }
        SyncDb::from_tar_bytes(repo, &builder.into_inner().unwrap()).unwrap()
    }

    #[test]
    fn earlier_repo_shadows_later() {
        let core = db_from(&[("foo", "2.0-1", "")], "core");
        let extra = db_from(&[("foo", "1.0-1", ""), ("bar", "1.0-1", "")], "extra");
        let repos = OfficialRepos::from_dbs(vec![core, extra]);
        let got = repos.get(&["foo".into(), "bar".into()]).unwrap();
        let foo = got.iter().find(|p| p.name == "foo").unwrap();
        assert_eq!(foo.version.to_string(), "2.0-1", "core's foo must win");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn missing_names_are_absent_not_errors() {
        let repos = OfficialRepos::from_dbs(vec![db_from(&[("foo", "1.0-1", "")], "core")]);
        let got = repos.get(&["foo".into(), "nope".into()]).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn providers_found_across_repos() {
        let core = db_from(&[("bash", "5.2-1", "%PROVIDES%\nsh\n")], "core");
        let extra = db_from(&[("zsh", "5.9-1", "%PROVIDES%\nsh\n")], "extra");
        let repos = OfficialRepos::from_dbs(vec![core, extra]);
        let providers = repos.find_providers("sh").unwrap();
        let names: Vec<_> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["bash", "zsh"]);
    }
}
