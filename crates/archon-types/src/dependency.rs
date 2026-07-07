use std::fmt;

use serde::{Deserialize, Serialize};

use crate::version::{Version, VersionParseError};

/// A dependency specification as written in a PKGBUILD or repo database:
/// `glibc`, `python>=3.11`, `libfoo.so=2-64`, ...
///
/// A dependency is satisfied by a package that either *is* `name` or
/// `provides` it, subject to the optional version constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub constraint: Option<VersionConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionConstraint {
    pub op: ConstraintOp,
    pub version: Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintOp {
    /// `=`
    Eq,
    /// `>=`
    Ge,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `<`
    Lt,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DependencyParseError {
    #[error("empty dependency string")]
    Empty,
    #[error("bad version in dependency {spec:?}: {source}")]
    BadVersion {
        spec: String,
        source: VersionParseError,
    },
}

impl Dependency {
    /// Parse a dependency spec string, splitting on the first comparison
    /// operator. Operator search order matters: `>=`/`<=` before `>`/`<`.
    pub fn parse(spec: &str) -> Result<Self, DependencyParseError> {
        if spec.is_empty() {
            return Err(DependencyParseError::Empty);
        }
        for (token, op) in [
            (">=", ConstraintOp::Ge),
            ("<=", ConstraintOp::Le),
            (">", ConstraintOp::Gt),
            ("<", ConstraintOp::Lt),
            ("=", ConstraintOp::Eq),
        ] {
            if let Some((name, ver)) = spec.split_once(token) {
                let version =
                    Version::parse(ver).map_err(|source| DependencyParseError::BadVersion {
                        spec: spec.to_owned(),
                        source,
                    })?;
                return Ok(Dependency {
                    name: name.to_owned(),
                    constraint: Some(VersionConstraint { op, version }),
                });
            }
        }
        Ok(Dependency {
            name: spec.to_owned(),
            constraint: None,
        })
    }

    /// Whether `version` satisfies this dependency's constraint (a missing
    /// constraint is satisfied by anything). Built on [`Version::vercmp`].
    pub fn is_satisfied_by(&self, version: &Version) -> bool {
        match &self.constraint {
            None => true,
            Some(c) => {
                let ord = version.vercmp(&c.version);
                match c.op {
                    ConstraintOp::Eq => ord == std::cmp::Ordering::Equal,
                    ConstraintOp::Ge => ord != std::cmp::Ordering::Less,
                    ConstraintOp::Le => ord != std::cmp::Ordering::Greater,
                    ConstraintOp::Gt => ord == std::cmp::Ordering::Greater,
                    ConstraintOp::Lt => ord == std::cmp::Ordering::Less,
                }
            }
        }
    }

    /// Whether a `provides=()` entry satisfies this dependency, per libalpm
    /// rules: names must match, an unversioned dep accepts any provide, and
    /// a *versioned* dep is only satisfied by a *versioned* provide whose
    /// version passes the constraint (a bare `provides=(foo)` never
    /// satisfies `foo>=2`).
    pub fn satisfied_by_provide(&self, provide: &Dependency) -> bool {
        if self.name != provide.name {
            return false;
        }
        match (&self.constraint, &provide.constraint) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(_), Some(pc)) => self.is_satisfied_by(&pc.version),
        }
    }
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(c) = &self.constraint {
            let op = match c.op {
                ConstraintOp::Eq => "=",
                ConstraintOp::Ge => ">=",
                ConstraintOp::Le => "<=",
                ConstraintOp::Gt => ">",
                ConstraintOp::Lt => "<",
            };
            write!(f, "{op}{}", c.version)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unversioned() {
        let d = Dependency::parse("glibc").unwrap();
        assert_eq!(d.name, "glibc");
        assert!(d.constraint.is_none());
    }

    #[test]
    fn parses_ge_constraint() {
        let d = Dependency::parse("python>=3.11").unwrap();
        assert_eq!(d.name, "python");
        let c = d.constraint.unwrap();
        assert_eq!(c.op, ConstraintOp::Ge);
        assert_eq!(c.version.pkgver, "3.11");
    }

    #[test]
    fn ge_is_not_gt_plus_eq() {
        let d = Dependency::parse("foo>=1").unwrap();
        assert_eq!(d.constraint.unwrap().op, ConstraintOp::Ge);
    }

    fn ver(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn unconstrained_dep_accepts_anything() {
        assert!(Dependency::parse("foo").unwrap().is_satisfied_by(&ver("0.0.1")));
    }

    #[test]
    fn constraint_ops_match_vercmp() {
        let cases = [
            ("foo>=1.5", "1.5", true),
            ("foo>=1.5", "1.4", false),
            ("foo>=1.5", "2.0", true),
            ("foo>1.5", "1.5", false),
            ("foo>1.5", "1.6", true),
            ("foo<=1.5", "1.5", true),
            ("foo<=1.5", "1.6", false),
            ("foo<1.5", "1.4", true),
            ("foo<1.5", "1.5", false),
            ("foo=1.5", "1.5", true),
            ("foo=1.5", "1.5.1", false),
        ];
        for (spec, candidate, expected) in cases {
            let d = Dependency::parse(spec).unwrap();
            assert_eq!(
                d.is_satisfied_by(&ver(candidate)),
                expected,
                "{spec} vs {candidate}"
            );
        }
    }

    #[test]
    fn eq_constraint_without_pkgrel_matches_any_pkgrel() {
        let d = Dependency::parse("foo=1.5").unwrap();
        assert!(d.is_satisfied_by(&ver("1.5-3")));
    }

    #[test]
    fn versioned_provide_satisfies_versioned_dep() {
        let dep = Dependency::parse("jdk>=17").unwrap();
        assert!(dep.satisfied_by_provide(&Dependency::parse("jdk=21").unwrap()));
        assert!(!dep.satisfied_by_provide(&Dependency::parse("jdk=11").unwrap()));
    }

    #[test]
    fn bare_provide_never_satisfies_versioned_dep() {
        let dep = Dependency::parse("jdk>=17").unwrap();
        assert!(!dep.satisfied_by_provide(&Dependency::parse("jdk").unwrap()));
        // ...but satisfies an unversioned one.
        let bare = Dependency::parse("jdk").unwrap();
        assert!(bare.satisfied_by_provide(&Dependency::parse("jdk").unwrap()));
    }

    #[test]
    fn provide_name_must_match() {
        let dep = Dependency::parse("libgl").unwrap();
        assert!(!dep.satisfied_by_provide(&Dependency::parse("libegl").unwrap()));
    }
}
