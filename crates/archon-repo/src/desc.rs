//! Parser for the `desc` file format inside sync databases.
//!
//! The format is line-based sections:
//!
//! ```text
//! %NAME%
//! firefox
//!
//! %VERSION%
//! 128.0-1
//!
//! %DEPENDS%
//! gtk3
//! libpulse
//! ```
//!
//! Fields we care about in Phase 1: NAME, VERSION, DESC, DEPENDS,
//! CONFLICTS, PROVIDES. Unknown fields are skipped, not errors — the format
//! grows over time and old archon versions must keep working.

use std::collections::HashMap;

use archon_types::{
    Dependency, DependencyParseError, PackageMeta, PackageSource, Version, VersionParseError,
};

#[derive(Debug, thiserror::Error)]
pub enum DescParseError {
    #[error("desc entry is missing required field %{0}%")]
    MissingField(&'static str),
    #[error("bad version: {0}")]
    BadVersion(#[from] VersionParseError),
    #[error("bad dependency spec in %{field}%: {source}")]
    BadDependency {
        field: String,
        source: DependencyParseError,
    },
}

/// Parse one desc file's contents into package metadata.
/// `repo` is the sync db it came from (for [`PackageSource::Official`]).
pub fn parse(repo: &str, contents: &str) -> Result<PackageMeta, DescParseError> {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current: Option<&str> = None;
    for line in contents.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            current = None;
        } else if let Some(field) = line.strip_prefix('%').and_then(|l| l.strip_suffix('%')) {
            current = Some(field);
            sections.entry(field).or_default();
        } else if let Some(field) = current {
            sections.get_mut(field).expect("section started").push(line);
        }
        // Lines before any %FIELD% header are silently ignored.
    }

    let single = |field: &'static str| -> Result<&str, DescParseError> {
        sections
            .get(field)
            .and_then(|v| v.first())
            .copied()
            .ok_or(DescParseError::MissingField(field))
    };
    let deps = |field: &'static str| -> Result<Vec<Dependency>, DescParseError> {
        sections
            .get(field)
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|s| {
                Dependency::parse(s).map_err(|source| DescParseError::BadDependency {
                    field: field.to_owned(),
                    source,
                })
            })
            .collect()
    };

    Ok(PackageMeta {
        name: single("NAME")?.to_owned(),
        version: Version::parse(single("VERSION")?)?,
        description: sections
            .get("DESC")
            .and_then(|v| v.first())
            .map(|s| (*s).to_owned()),
        source: PackageSource::Official { repo: repo.to_owned() },
        depends: deps("DEPENDS")?,
        // Sync dbs don't carry makedepends for prebuilt packages.
        make_depends: vec![],
        conflicts: deps("CONFLICTS")?,
        provides: deps("PROVIDES")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "\
%FILENAME%
firefox-128.0-1-x86_64.pkg.tar.zst

%NAME%
firefox

%VERSION%
128.0-1

%DESC%
Fast web browser

%DEPENDS%
gtk3
libpulse
nss>=3.100

%PROVIDES%
browser
";

    #[test]
    fn parses_full_entry() {
        let pkg = parse("extra", FIREFOX).unwrap();
        assert_eq!(pkg.name, "firefox");
        assert_eq!(pkg.version.to_string(), "128.0-1");
        assert_eq!(pkg.description.as_deref(), Some("Fast web browser"));
        assert_eq!(pkg.depends.len(), 3);
        assert_eq!(pkg.depends[2].to_string(), "nss>=3.100");
        assert_eq!(pkg.provides.len(), 1);
        assert!(pkg.conflicts.is_empty());
        assert_eq!(
            pkg.source,
            PackageSource::Official { repo: "extra".into() }
        );
    }

    #[test]
    fn missing_name_is_an_error() {
        let err = parse("core", "%VERSION%\n1.0-1\n").unwrap_err();
        assert!(matches!(err, DescParseError::MissingField("NAME")));
    }

    #[test]
    fn missing_version_is_an_error() {
        let err = parse("core", "%NAME%\nfoo\n").unwrap_err();
        assert!(matches!(err, DescParseError::MissingField("VERSION")));
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let contents = "%NAME%\nfoo\n\n%VERSION%\n1.0-1\n\n%SHINYNEWFIELD%\nstuff\n";
        let pkg = parse("core", contents).unwrap();
        assert_eq!(pkg.name, "foo");
    }

    #[test]
    fn empty_dep_sections_are_empty_not_missing() {
        let pkg = parse("core", "%NAME%\nfoo\n\n%VERSION%\n1.0-1\n\n%DEPENDS%\n\n").unwrap();
        assert!(pkg.depends.is_empty());
    }
}
