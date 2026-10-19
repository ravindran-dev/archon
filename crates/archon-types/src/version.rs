use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A pacman package version: `[epoch:]pkgver[-pkgrel]`.
///
/// Ordering follows libalpm's `vercmp(8)` rules so archon agrees with pacman
/// about which of two versions is newer:
///
/// 1. A higher `epoch` always wins, regardless of `pkgver`.
/// 2. `pkgver` is compared segment-wise: runs of digits compare numerically,
///    runs of letters compare lexically, and numeric segments always beat
///    alphabetic ones. A trailing alphabetic suffix sorts *older* than the
///    bare version (`1.0rc < 1.0`), but a suffix after a separator sorts
///    *newer* (`1.0 < 1.0.a < 1.0.1`) — see the man page's example chain.
/// 3. `pkgrel` breaks ties, compared the same way. A version without a
///    pkgrel matches any pkgrel (pacman treats `1.0` as satisfying `=1.0-2`).
///
/// Note there is deliberately no `Ord` impl: the pkgrel rule makes the
/// relation non-transitive (`1.0 == 1.0-1` and `1.0 == 1.0-2`, yet
/// `1.0-1 < 1.0-2`), which would violate `Ord`'s contract. Use
/// [`Version::vercmp`] explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VersionBackend {
    #[default]
    Alpm,
    Dpkg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub epoch: u64,
    pub pkgver: String,
    pub pkgrel: Option<String>,
    #[serde(default)]
    pub backend: VersionBackend,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionParseError {
    #[error("empty version string")]
    Empty,
    #[error("invalid epoch in version {0:?}")]
    InvalidEpoch(String),
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("cannot compare package versions from different backends: {left:?} and {right:?}")]
pub struct VersionCompareError {
    pub left: VersionBackend,
    pub right: VersionBackend,
}

impl Version {
    /// Parse a full version string as it appears in repo databases and the
    /// AUR RPC, e.g. `2:1.4.3-1`, `20240101-2`, or a bare `1.0`.
    pub fn parse(s: &str) -> Result<Self, VersionParseError> {
        if s.is_empty() {
            return Err(VersionParseError::Empty);
        }
        let (epoch, rest) = match s.split_once(':') {
            Some((e, rest)) => (
                e.parse()
                    .map_err(|_| VersionParseError::InvalidEpoch(s.to_owned()))?,
                rest,
            ),
            None => (0, s),
        };
        let (pkgver, pkgrel) = match rest.rsplit_once('-') {
            Some((v, r)) => (v.to_owned(), Some(r.to_owned())),
            None => (rest.to_owned(), None),
        };
        Ok(Version {
            epoch,
            pkgver,
            pkgrel,
            backend: VersionBackend::Alpm,
        })
    }

    pub fn parse_dpkg(s: &str) -> Result<Self, VersionParseError> {
        if s.is_empty() {
            return Err(VersionParseError::Empty);
        }
        let (epoch, rest) = match s.find(':') {
            Some(idx) => (
                s[..idx]
                    .parse()
                    .map_err(|_| VersionParseError::InvalidEpoch(s.to_owned()))?,
                &s[idx + 1..],
            ),
            None => (0, s),
        };
        let (pkgver, pkgrel) = match rest.rfind('-') {
            Some(idx) => (rest[..idx].to_owned(), Some(rest[idx + 1..].to_owned())),
            None => (rest.to_owned(), None),
        };
        Ok(Version {
            epoch,
            pkgver,
            pkgrel,
            backend: VersionBackend::Dpkg,
        })
    }

    /// libalpm-compatible comparison (see type-level docs for the rules).
    pub fn vercmp(&self, other: &Self) -> Result<Ordering, VersionCompareError> {
        if self.backend != other.backend {
            return Err(VersionCompareError {
                left: self.backend,
                right: other.backend,
            });
        }

        Ok(self
            .epoch
            .cmp(&other.epoch)
            .then_with(|| {
                if self.backend == VersionBackend::Dpkg {
                    dpkgvercmp(&self.pkgver, &other.pkgver)
                } else {
                    rpmvercmp(&self.pkgver, &other.pkgver)
                }
            })
            .then_with(|| match (&self.pkgrel, &other.pkgrel) {
                (Some(a), Some(b)) => {
                    if self.backend == VersionBackend::Dpkg {
                        dpkgvercmp(a, b)
                    } else {
                        rpmvercmp(a, b)
                    }
                }
                (Some(a), None) => {
                    if self.backend == VersionBackend::Dpkg {
                        dpkgvercmp(a, "")
                    } else {
                        Ordering::Equal
                    }
                }
                (None, Some(b)) => {
                    if other.backend == VersionBackend::Dpkg {
                        dpkgvercmp("", b)
                    } else {
                        Ordering::Equal
                    }
                }
                _ => Ordering::Equal,
            }))
    }
}

fn dpkg_order(c: u8) -> i32 {
    if c.is_ascii_alphabetic() {
        c as i32
    } else if c == b'~' {
        -1
    } else if c == 0 {
        0
    } else {
        (c as i32) + 256
    }
}

/// Port of dpkg's version comparison algorithm.
fn dpkgvercmp(a: &str, b: &str) -> Ordering {
    let mut a = a.as_bytes();
    let mut b = b.as_bytes();
    while !a.is_empty() || !b.is_empty() {
        let mut i = 0;
        let mut j = 0;
        while i < a.len() && !a[i].is_ascii_digit() {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_digit() {
            j += 1;
        }

        let a_chars = &a[..i];
        let b_chars = &b[..j];

        // Compare non-digit characters
        let max_len = std::cmp::max(a_chars.len(), b_chars.len());
        for k in 0..max_len {
            let ac = if k < a_chars.len() { a_chars[k] } else { 0 };
            let bc = if k < b_chars.len() { b_chars[k] } else { 0 };
            let ord_a = dpkg_order(ac);
            let ord_b = dpkg_order(bc);
            if ord_a != ord_b {
                return ord_a.cmp(&ord_b);
            }
        }

        a = &a[i..];
        b = &b[j..];

        i = 0;
        j = 0;
        while i < a.len() && a[i].is_ascii_digit() {
            i += 1;
        }
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }

        let mut a_digits = &a[..i];
        let mut b_digits = &b[..j];

        while a_digits.first() == Some(&b'0') {
            a_digits = &a_digits[1..];
        }
        while b_digits.first() == Some(&b'0') {
            b_digits = &b_digits[1..];
        }

        let ord = a_digits
            .len()
            .cmp(&b_digits.len())
            .then_with(|| a_digits.cmp(b_digits));
        if ord != Ordering::Equal {
            return ord;
        }

        a = &a[i..];
        b = &b[j..];
    }
    Ordering::Equal
}

/// Port of libalpm's `rpmvercmp` (alpm's fork of RPM's algorithm — the tail
/// handling differs from RPM proper: an alphabetic leftover *loses*).
///
/// Walks both strings in lockstep, one segment at a time. A segment is a
/// maximal run of digits or of letters; non-alphanumeric bytes are
/// separators. Digits beat letters; digit runs compare as numbers (leading
/// zeros stripped, longer run of significant digits wins).
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        // Skip separators, remembering how many we skipped on each side.
        let (sep_start_a, sep_start_b) = (i, j);
        while i < a.len() && !a[i].is_ascii_alphanumeric() {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_alphanumeric() {
            j += 1;
        }

        if i >= a.len() || j >= b.len() {
            break;
        }
        // Different separator run lengths decide immediately ("1.0" < "1..0").
        let sep_cmp = (i - sep_start_a).cmp(&(j - sep_start_b));
        if sep_cmp != Ordering::Equal {
            return sep_cmp;
        }

        // Take the next segment from `a`; its type (digit vs alpha) dictates
        // what we try to take from `b`.
        let numeric = a[i].is_ascii_digit();
        let (seg_start_a, seg_start_b) = (i, j);
        if numeric {
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            while i < a.len() && a[i].is_ascii_alphabetic() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
        }
        let seg_a = &a[seg_start_a..i];
        let seg_b = &b[seg_start_b..j];

        // `seg_a` is never empty here; if `b`'s segment is the other type,
        // numeric beats alphabetic.
        if seg_b.is_empty() {
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        let ord = if numeric {
            let ta = trim_leading_zeros(seg_a);
            let tb = trim_leading_zeros(seg_b);
            // More significant digits = bigger number; equal length falls
            // back to lexical compare, which is numeric compare for
            // equal-length digit strings.
            ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb))
        } else {
            seg_a.cmp(seg_b)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }

    // All shared segments compared equal — the tail decides.
    match (i < a.len(), j < b.len()) {
        (false, false) => Ordering::Equal,
        // alpm quirk: a leftover *alphabetic* suffix is a pre-release and
        // loses ("1.0rc" < "1.0"); any other leftover (digits, separators)
        // wins ("1.0.1" > "1.0", "1.0.a" > "1.0").
        (false, true) => {
            if b[j].is_ascii_alphabetic() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (true, false) => {
            if a[i].is_ascii_alphabetic() {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        // The loop only exits with at least one side exhausted.
        (true, true) => unreachable!("rpmvercmp loop invariant"),
    }
}

fn trim_leading_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[start..]
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch > 0 {
            write!(f, "{}:", self.epoch)?;
        }
        write!(f, "{}", self.pkgver)?;
        if let Some(rel) = &self.pkgrel {
            write!(f, "-{rel}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::{Equal, Greater, Less};

    #[test]
    fn mixed_backend_comparison_returns_without_panicking() {
        let arch = Version::parse("1").unwrap();
        let deb = Version::parse_dpkg("1").unwrap();
        assert!(std::panic::catch_unwind(|| arch.vercmp(&deb)).is_ok());
        assert_eq!(
            arch.vercmp(&deb),
            Err(VersionCompareError {
                left: VersionBackend::Alpm,
                right: VersionBackend::Dpkg,
            })
        );
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_epoch_ver_rel() {
        let ver = v("2:1.4.3-1");
        assert_eq!(ver.epoch, 2);
        assert_eq!(ver.pkgver, "1.4.3");
        assert_eq!(ver.pkgrel.as_deref(), Some("1"));
    }

    #[test]
    fn parses_bare_version() {
        let ver = v("1.0");
        assert_eq!(ver.epoch, 0);
        assert_eq!(ver.pkgrel, None);
    }

    #[test]
    fn display_roundtrips() {
        for s in ["1.0", "1.0-1", "2:1.4.3-1", "20240101-2"] {
            assert_eq!(v(s).to_string(), s);
        }
    }

    /// The ordered chain straight from the vercmp(8) man page. Every
    /// adjacent pair (and by transitivity, every pair) must sort in order.
    #[test]
    fn man_page_alpha_chain() {
        let chain = [
            "1.0a", "1.0alpha", "1.0b", "1.0beta", "1.0rc", "1.0", "1.0.a", "1.0.1",
        ];
        assert_chain_ascending(&chain);
    }

    #[test]
    fn man_page_numeric_chain() {
        let chain = ["1", "1.0", "1.1", "1.1.1", "1.2", "2.0", "3.0.0"];
        assert_chain_ascending(&chain);
    }

    fn assert_chain_ascending(chain: &[&str]) {
        for pair in chain.windows(2) {
            assert_eq!(
                v(pair[0]).vercmp(&v(pair[1])).unwrap(),
                Less,
                "expected {} < {}",
                pair[0],
                pair[1]
            );
            assert_eq!(
                v(pair[1]).vercmp(&v(pair[0])).unwrap(),
                Greater,
                "expected {} > {}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn alpm_tilde_is_a_separator_not_debian_prerelease() {
        assert_eq!(v("1.0~rc1").vercmp(&v("1.0")).unwrap(), Greater);
        assert_eq!(v("1.0~~").vercmp(&v("1.0~")).unwrap(), Equal);
    }

    #[test]
    fn numeric_compares_by_value_not_lexically() {
        assert_eq!(v("10.0").vercmp(&v("2.0")).unwrap(), Greater);
        assert_eq!(v("1.010").vercmp(&v("1.9")).unwrap(), Greater);
    }

    #[test]
    fn leading_zeros_are_insignificant() {
        assert_eq!(v("1.001").vercmp(&v("1.1")).unwrap(), Equal);
        assert_eq!(v("01").vercmp(&v("1")).unwrap(), Equal);
    }

    #[test]
    fn epoch_dominates_everything() {
        assert_eq!(v("1:0.1").vercmp(&v("999.999-99")).unwrap(), Greater);
        assert_eq!(v("2:1.0").vercmp(&v("1:99.0")).unwrap(), Greater);
    }

    #[test]
    fn pkgrel_breaks_ties() {
        assert_eq!(v("1.0-1").vercmp(&v("1.0-2")).unwrap(), Less);
        assert_eq!(v("1.0-2").vercmp(&v("1.0-1")).unwrap(), Greater);
        assert_eq!(v("1.0-1").vercmp(&v("1.0-1")).unwrap(), Equal);
    }

    #[test]
    fn missing_pkgrel_matches_any_pkgrel() {
        assert_eq!(v("1.0").vercmp(&v("1.0-2")).unwrap(), Equal);
        assert_eq!(v("1.0-2").vercmp(&v("1.0")).unwrap(), Equal);
    }

    #[test]
    fn numeric_segment_beats_alphabetic() {
        assert_eq!(v("1.0.1").vercmp(&v("1.0.a")).unwrap(), Greater);
        assert_eq!(v("2024").vercmp(&v("beta")).unwrap(), Greater);
    }

    #[test]
    fn separator_runs_compare_by_length() {
        assert_eq!(v("1..0").vercmp(&v("1.0")).unwrap(), Greater);
        assert_eq!(v("1.0").vercmp(&v("1..0")).unwrap(), Less);
    }

    #[test]
    fn pkgrel_with_subrelease() {
        assert_eq!(v("1.0-1.2").vercmp(&v("1.0-1.1")).unwrap(), Greater);
    }

    #[test]
    fn debian_tilde_sorts_older_than_anything() {
        // This test specifies Debian behavior; its versions must use Dpkg.
        let v = |text: &str| Version::parse_dpkg(text).unwrap();
        assert_eq!(v("1").backend, VersionBackend::Dpkg);
        assert_eq!(v("1.0~rc1").vercmp(&v("1.0")).unwrap(), Less);
        assert_eq!(v("1.0~rc1").vercmp(&v("1.0.1")).unwrap(), Less);
        assert_eq!(v("1.0~rc1").vercmp(&v("1.0~rc2")).unwrap(), Less);
        assert_eq!(v("1.0~rc2").vercmp(&v("1.0~rc1")).unwrap(), Greater);
        assert_eq!(v("1.0~rc1~2").vercmp(&v("1.0~rc1")).unwrap(), Less);
        assert_eq!(v("1~rc1").vercmp(&v("1~rc1")).unwrap(), Equal);
    }

    fn dv(s: &str) -> Version {
        Version::parse_dpkg(s).unwrap()
    }

    #[test]
    fn dpkg_version_comparisons() {
        assert_eq!(dv("1.0~rc1").vercmp(&dv("1.0~rc2")).unwrap(), Less);
        assert_eq!(dv("1.0~~").vercmp(&dv("1.0~")).unwrap(), Less);
        assert_eq!(dv("1:2.0").vercmp(&dv("2.0~rc1")).unwrap(), Greater);
        assert_eq!(dv("1.0~rc1~2").vercmp(&dv("1.0~rc1")).unwrap(), Less);
        assert_eq!(dv("1.0").vercmp(&dv("1.0~rc1")).unwrap(), Greater);
        assert_eq!(dv("1.0-1").vercmp(&dv("1.0-2")).unwrap(), Less);
        assert_eq!(dv("1.0a").vercmp(&dv("1.0")).unwrap(), Greater);
    }
}
