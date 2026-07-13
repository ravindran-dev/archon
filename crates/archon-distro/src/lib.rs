//! Distro detection and native-package-manager backends, for archon's
//! non-Arch install/remove/search paths.
//!
//! archon's own dependency resolver, AUR client, and sandboxed, eBPF-
//! monitored build pipeline are Arch-specific by design — pacman's sync
//! database format, the AUR, and PKGBUILD/makepkg don't have equivalents
//! on other distros, and reimplementing full dependency resolution against
//! every distro's own package database format (dpkg's status file, the RPM
//! database, ...) would be its own multi-month project per format. Instead,
//! off Arch, archon detects the host distro and delegates resolution and
//! installation entirely to its native package manager — archon's value
//! there is detection and a consistent CLI, not reinventing apt or dnf.

mod backend;

pub use backend::{
    ApkBackend, AptBackend, Backend, DnfBackend, EmergeBackend, NixBackend, PacmanBackend, XbpsBackend,
    ZypperBackend,
};

use std::path::{Path, PathBuf};

/// The distro families archon currently has a native backend for, plus a
/// catch-all for everything else. `Unknown` carries the raw `ID` from
/// `/etc/os-release` so error messages can name what was actually detected
/// instead of just saying "not supported."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Arch,
    Debian,
    Fedora,
    Suse,
    Alpine,
    NixOs,
    Void,
    Gentoo,
    Unknown(String),
}

impl Distro {
    /// Reads `/etc/os-release`, or `$ARCHON_OS_RELEASE` if set (test
    /// hermeticity — every CLI integration test defaults to a fixture
    /// declaring Arch, since that's what the existing suite exercises,
    /// regardless of whatever distro actually runs the tests).
    pub fn detect() -> Self {
        let path = std::env::var_os("ARCHON_OS_RELEASE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/etc/os-release"));
        Self::detect_at(&path)
    }

    /// As [`Distro::detect`], but reads an explicit path — the part that's
    /// actually worth unit testing directly, without going through an
    /// environment variable (which would mean mutating process-global
    /// state from a test, racing every other test in the same binary).
    pub fn detect_at(path: &Path) -> Self {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return Distro::Unknown("unreadable /etc/os-release".to_string());
        };
        Self::from_os_release(&contents)
    }

    /// Checks `ID` first, then `ID_LIKE` for derivatives (e.g. Manjaro's
    /// `ID=manjaro` / `ID_LIKE=arch`, Pop!_OS's `ID=pop` /
    /// `ID_LIKE="ubuntu debian"`) — derivatives declare their upstream
    /// family there specifically so tools like this can treat them the
    /// same way as upstream.
    fn from_os_release(contents: &str) -> Self {
        let fields = parse_os_release(contents);
        let id = fields.get("ID").cloned().unwrap_or_default();
        let id_like = fields.get("ID_LIKE").cloned().unwrap_or_default();
        let haystack = format!("{id} {id_like}");
        let has = |w: &str| haystack.split_whitespace().any(|word| word == w);

        if has("arch") {
            return Distro::Arch;
        }
        if has("debian") || has("ubuntu") {
            return Distro::Debian;
        }
        if has("fedora") || has("rhel") {
            return Distro::Fedora;
        }
        // openSUSE's ID is a compound word ("opensuse-tumbleweed",
        // "opensuse-leap"), so word-equality alone would miss it — but its
        // ID_LIKE is "opensuse suse", and SLES is ID=sles/ID_LIKE=suse, so
        // the word check covers both once the compound prefix is also
        // recognized.
        if has("suse") || has("opensuse") || id.starts_with("opensuse") {
            return Distro::Suse;
        }
        if has("alpine") {
            return Distro::Alpine;
        }
        if has("nixos") {
            return Distro::NixOs;
        }
        if has("void") {
            return Distro::Void;
        }
        if has("gentoo") {
            return Distro::Gentoo;
        }
        Distro::Unknown(if id.is_empty() { "unknown".to_string() } else { id })
    }

    pub fn is_arch(&self) -> bool {
        matches!(self, Distro::Arch)
    }

    /// The native backend for this distro, or `None` for anything archon
    /// doesn't have one for yet.
    pub fn backend(&self) -> Option<Box<dyn Backend>> {
        match self {
            Distro::Arch => Some(Box::new(PacmanBackend)),
            Distro::Debian => Some(Box::new(AptBackend)),
            Distro::Fedora => Some(Box::new(DnfBackend)),
            Distro::Suse => Some(Box::new(ZypperBackend)),
            Distro::Alpine => Some(Box::new(ApkBackend)),
            Distro::NixOs => Some(Box::new(NixBackend)),
            Distro::Void => Some(Box::new(XbpsBackend)),
            Distro::Gentoo => Some(Box::new(EmergeBackend)),
            Distro::Unknown(_) => None,
        }
    }
}

impl std::fmt::Display for Distro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Distro::Arch => write!(f, "Arch Linux"),
            Distro::Debian => write!(f, "Debian/Ubuntu"),
            Distro::Fedora => write!(f, "Fedora/RHEL"),
            Distro::Suse => write!(f, "openSUSE/SLE"),
            Distro::Alpine => write!(f, "Alpine Linux"),
            Distro::NixOs => write!(f, "NixOS"),
            Distro::Void => write!(f, "Void Linux"),
            Distro::Gentoo => write!(f, "Gentoo"),
            Distro::Unknown(id) => write!(f, "{id}"),
        }
    }
}

/// Minimal `KEY=value` parser for `/etc/os-release` — values may be quoted
/// (`ID="ubuntu"`) or bare (`ID=arch`); only the two fields archon actually
/// reads (`ID`, `ID_LIKE`) matter, everything else in the file is fine to
/// skip.
fn parse_os_release(contents: &str) -> std::collections::HashMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = value.trim().trim_matches('"');
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_arch_by_id() {
        assert_eq!(Distro::from_os_release("ID=arch\nNAME=\"Arch Linux\"\n"), Distro::Arch);
    }

    #[test]
    fn detects_ubuntu_by_id() {
        assert_eq!(Distro::from_os_release("ID=ubuntu\nID_LIKE=debian\n"), Distro::Debian);
    }

    #[test]
    fn detects_debian_derivative_by_id_like() {
        // Pop!_OS: its own ID, but ID_LIKE names its real upstream family.
        assert_eq!(Distro::from_os_release("ID=pop\nID_LIKE=\"ubuntu debian\"\n"), Distro::Debian);
    }

    #[test]
    fn detects_arch_derivative_by_id_like() {
        assert_eq!(Distro::from_os_release("ID=manjaro\nID_LIKE=arch\n"), Distro::Arch);
    }

    #[test]
    fn detects_fedora_family() {
        assert_eq!(Distro::from_os_release("ID=fedora\n"), Distro::Fedora);
        assert_eq!(Distro::from_os_release("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n"), Distro::Fedora);
    }

    #[test]
    fn unknown_distro_carries_its_id() {
        let d = Distro::from_os_release("ID=slackware\n");
        assert_eq!(d, Distro::Unknown("slackware".to_string()));
        assert!(!d.is_arch());
    }

    #[test]
    fn detects_suse_family() {
        // Tumbleweed: compound ID, family declared in ID_LIKE.
        assert_eq!(
            Distro::from_os_release("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n"),
            Distro::Suse
        );
        assert_eq!(Distro::from_os_release("ID=sles\nID_LIKE=suse\n"), Distro::Suse);
        // Older/hypothetical bare compound ID with no ID_LIKE at all.
        assert_eq!(Distro::from_os_release("ID=opensuse-leap\n"), Distro::Suse);
    }

    #[test]
    fn detects_alpine_nixos_void_gentoo() {
        assert_eq!(Distro::from_os_release("ID=alpine\n"), Distro::Alpine);
        assert_eq!(Distro::from_os_release("ID=nixos\n"), Distro::NixOs);
        assert_eq!(Distro::from_os_release("ID=void\n"), Distro::Void);
        assert_eq!(Distro::from_os_release("ID=gentoo\n"), Distro::Gentoo);
    }

    #[test]
    fn missing_file_is_unknown_not_a_panic() {
        let d = Distro::detect_at(Path::new("/nonexistent/path/archon-test-xyz"));
        assert!(matches!(d, Distro::Unknown(_)));
    }

    #[test]
    fn backend_names_match_distro() {
        assert_eq!(Distro::Arch.backend().unwrap().name(), "pacman");
        assert_eq!(Distro::Debian.backend().unwrap().name(), "apt");
        assert_eq!(Distro::Fedora.backend().unwrap().name(), "dnf");
        assert_eq!(Distro::Suse.backend().unwrap().name(), "zypper");
        assert_eq!(Distro::Alpine.backend().unwrap().name(), "apk");
        assert_eq!(Distro::NixOs.backend().unwrap().name(), "nix-env");
        assert_eq!(Distro::Void.backend().unwrap().name(), "xbps");
        assert_eq!(Distro::Gentoo.backend().unwrap().name(), "emerge");
        assert!(Distro::Unknown("x".into()).backend().is_none());
    }

    #[test]
    fn display_names_are_human_readable() {
        assert_eq!(Distro::Arch.to_string(), "Arch Linux");
        assert_eq!(Distro::Debian.to_string(), "Debian/Ubuntu");
        assert_eq!(Distro::Unknown("gentoo".into()).to_string(), "gentoo");
    }
}
