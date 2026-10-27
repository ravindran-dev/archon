//! Native adapter contract. Distro-specific argv and policy live in separate
//! modules; this module contains only shared types and compatibility tests.

/// Inventory versions retain their native version semantics (including RPM EVR).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub architecture: String,
}

/// Operations beyond the original install/remove/search API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Upgrade,
    Refresh,
    Info,
    List,
    Reinstall,
    Repositories,
    Check,
    History,
    Provides,
}

impl Operation {
    pub const ALL: [Self; 9] = [
        Self::Upgrade,
        Self::Refresh,
        Self::Info,
        Self::List,
        Self::Reinstall,
        Self::Repositories,
        Self::Check,
        Self::History,
        Self::Provides,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Upgrade => "upgrade",
            Self::Refresh => "refresh",
            Self::Info => "info",
            Self::List => "list",
            Self::Reinstall => "reinstall",
            Self::Repositories => "repos",
            Self::Check => "check",
            Self::History => "history",
            Self::Provides => "provides",
        }
    }

    pub fn mutates(self) -> bool {
        matches!(self, Self::Upgrade | Self::Refresh | Self::Reinstall)
    }

    pub fn requires_packages(self) -> bool {
        matches!(self, Self::Info | Self::Reinstall | Self::Provides)
    }
}

/// Native package-management adapter. Each distro owns its commands,
/// capabilities and host restrictions. Native engines remain authoritative
/// for transaction solving; common CLI workflows do not contain native flags.
pub trait Backend: std::fmt::Debug {
    /// Optional native transaction engine. Other adapters remain explicitly unsupported.
    fn transaction_engine(&self) -> Option<&'static str> {
        None
    }

    fn transaction_bridge(&self) -> Option<&'static str> {
        None
    }

    /// Short, lowercase identifier used in messages ("pacman", "apt", "dnf").
    fn name(&self) -> &'static str;

    /// argv (including the program name) to install `names`. `assume_yes`
    /// maps to the native tool's own confirmation-skip flag — archon never
    /// forces this silently; it only appears when the caller's own
    /// `--yes` was set, mirroring how the Arch path already treats
    /// `--yes` as "skip archon's own prompt," not "skip every prompt that
    /// exists anywhere."
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String>;

    /// argv to remove `names`.
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String>;

    /// argv to search for `term`. Output is the native tool's own, printed
    /// as-is — archon has no parsed package database to search
    /// structurally on non-Arch distros the way
    /// `archon_repo::OfficialRepos::search` does for pacman sync dbs.
    fn search_argv(&self, term: &str) -> Vec<String>;

    /// argv whose exit status (not its output) answers "is `name`
    /// installed?" — 0 for yes, nonzero for no, same convention every one
    /// of these native query tools already follows.
    fn is_installed_argv(&self, name: &str) -> Vec<String>;

    /// Extended native operations. An adapter opts in explicitly; adding a
    /// capability to one distro cannot silently change another adapter.
    fn operation_argv(
        &self,
        _operation: Operation,
        _names: &[String],
        _assume_yes: bool,
    ) -> Option<Vec<String>> {
        None
    }

    /// Optional machine-readable inventory, independently implemented by adapters.
    fn inventory_argv(&self) -> Option<Vec<String>> {
        None
    }

    fn parse_inventory(&self, _output: &str) -> Result<Vec<InstalledPackage>, String> {
        Err(format!(
            "{} has no structured inventory adapter",
            self.name()
        ))
    }

    fn operation_needs_root(&self, operation: Operation) -> bool {
        operation.mutates() && self.needs_root()
    }

    fn scope(&self) -> &'static str {
        "system packages"
    }

    fn operation_unavailable_reason(&self, _operation: Operation) -> Option<&'static str> {
        None
    }

    /// Runtime host facts are supplied by orchestration, policy by the adapter.
    fn mutation_restriction(&self, _ostree_booted: bool) -> Option<&'static str> {
        None
    }

    /// Whether install/remove must run under `sudo`. True for every
    /// system-wide package manager; `NixBackend` overrides it to false
    /// because `nix-env` installs into the invoking user's own profile —
    /// running it under sudo wouldn't just be unnecessary, it would
    /// install into *root's* profile instead of the user's.
    fn needs_root(&self) -> bool {
        true
    }
}

fn argv(parts: &[&str], names: &[String]) -> Vec<String> {
    parts
        .iter()
        .map(|s| s.to_string())
        .chain(names.iter().cloned())
        .collect()
}

mod arch;
pub use arch::PacmanBackend;
mod debian;
pub use debian::AptBackend;
mod fedora;
pub use fedora::DnfBackend;
mod suse;
pub use suse::ZypperBackend;
mod alpine;
pub use alpine::ApkBackend;
mod nix;
pub use nix::NixBackend;
mod void;
pub use void::XbpsBackend;
mod gentoo;
pub use gentoo::EmergeBackend;
mod rpm;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_install_argv() {
        assert_eq!(
            PacmanBackend.install_argv(&["firefox".into()], false),
            ["pacman", "-S", "--needed", "firefox"]
        );
    }

    #[test]
    fn pacman_install_argv_assume_yes() {
        assert_eq!(
            PacmanBackend.install_argv(&["firefox".into()], true),
            ["pacman", "-S", "--needed", "--noconfirm", "firefox"]
        );
    }

    #[test]
    fn apt_install_argv() {
        assert_eq!(
            AptBackend.install_argv(&["firefox".into(), "vim".into()], false),
            ["apt", "install", "firefox", "vim"]
        );
    }

    #[test]
    fn apt_install_argv_assume_yes() {
        assert_eq!(
            AptBackend.install_argv(&["firefox".into()], true),
            ["apt", "install", "-y", "firefox"]
        );
    }

    #[test]
    fn dnf_remove_argv() {
        assert_eq!(
            DnfBackend.remove_argv(&["firefox".into()], true),
            ["dnf", "remove", "-y", "firefox"]
        );
    }

    #[test]
    fn dnf_remove_argv_without_assume_yes() {
        assert_eq!(
            DnfBackend.remove_argv(&["firefox".into()], false),
            ["dnf", "remove", "firefox"]
        );
    }

    #[test]
    fn is_installed_argv_uses_native_query_tools() {
        assert_eq!(PacmanBackend.is_installed_argv("x"), ["pacman", "-Q", "x"]);
        assert_eq!(AptBackend.is_installed_argv("x"), ["dpkg", "-s", "x"]);
        assert_eq!(DnfBackend.is_installed_argv("x"), ["rpm", "-q", "x"]);
    }

    #[test]
    fn search_argv_per_backend() {
        assert_eq!(PacmanBackend.search_argv("vim"), ["pacman", "-Ss", "vim"]);
        assert_eq!(
            AptBackend.search_argv("vim"),
            ["apt-cache", "search", "vim"]
        );
        assert_eq!(DnfBackend.search_argv("vim"), ["dnf", "search", "vim"]);
        assert_eq!(
            ZypperBackend.search_argv("vim"),
            ["zypper", "search", "vim"]
        );
        assert_eq!(ApkBackend.search_argv("vim"), ["apk", "search", "vim"]);
        assert_eq!(NixBackend.search_argv("vim"), ["nix-env", "-qaP", "vim"]);
        assert_eq!(XbpsBackend.search_argv("vim"), ["xbps-query", "-Rs", "vim"]);
        assert_eq!(
            EmergeBackend.search_argv("vim"),
            ["emerge", "--search", "vim"]
        );
    }

    #[test]
    fn nix_installs_by_channel_attribute_and_removes_by_name() {
        assert_eq!(
            NixBackend.install_argv(&["firefox".into(), "vim".into()], false),
            ["nix-env", "-iA", "nixos.firefox", "nixos.vim"]
        );
        assert_eq!(
            NixBackend.remove_argv(&["firefox".into()], false),
            ["nix-env", "-e", "firefox"]
        );
    }

    #[test]
    fn only_nix_skips_root() {
        assert!(!NixBackend.needs_root());
        for b in [
            &PacmanBackend as &dyn Backend,
            &AptBackend,
            &DnfBackend,
            &ZypperBackend,
            &ApkBackend,
            &XbpsBackend,
            &EmergeBackend,
        ] {
            assert!(b.needs_root(), "{} must require root", b.name());
        }
    }

    #[test]
    fn zypper_assume_yes_uses_non_interactive_global_flag() {
        assert_eq!(
            ZypperBackend.install_argv(&["vim".into()], true),
            ["zypper", "--non-interactive", "install", "vim"]
        );
        assert_eq!(
            ZypperBackend.install_argv(&["vim".into()], false),
            ["zypper", "install", "vim"]
        );
    }

    #[test]
    fn apk_is_always_non_interactive() {
        // Same argv either way: apk has no prompt for --yes to skip.
        assert_eq!(
            ApkBackend.install_argv(&["vim".into()], false),
            ["apk", "add", "vim"]
        );
        assert_eq!(
            ApkBackend.install_argv(&["vim".into()], true),
            ["apk", "add", "vim"]
        );
        assert_eq!(
            ApkBackend.remove_argv(&["vim".into()], true),
            ["apk", "del", "vim"]
        );
    }

    #[test]
    fn xbps_argv() {
        assert_eq!(
            XbpsBackend.install_argv(&["vim".into()], false),
            ["xbps-install", "-S", "vim"]
        );
        assert_eq!(
            XbpsBackend.install_argv(&["vim".into()], true),
            ["xbps-install", "-Sy", "vim"]
        );
        assert_eq!(
            XbpsBackend.remove_argv(&["vim".into()], true),
            ["xbps-remove", "-Ry", "vim"]
        );
    }

    #[test]
    fn emerge_ask_flag_is_inverted_but_semantics_match() {
        // No --yes: user still gets a confirmation (emerge's own --ask).
        assert_eq!(
            EmergeBackend.install_argv(&["vim".into()], false),
            ["emerge", "--ask", "vim"]
        );
        // --yes: no prompt anywhere, same as -y elsewhere.
        assert_eq!(
            EmergeBackend.install_argv(&["vim".into()], true),
            ["emerge", "vim"]
        );
    }
}
