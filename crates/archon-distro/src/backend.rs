//! Native package-manager backends: pure argv construction, no execution —
//! the same testing philosophy `archon_sandbox::SandboxPolicy::bwrap_args`
//! uses: build the command, verify the command in a unit test, and let the
//! `archon` CLI crate actually run it (with the same spinner/confirmation
//! UX shell that wraps every side-effecting action archon takes).

/// One distro's native package manager: enough surface for archon to
/// install/remove/search without reimplementing dependency resolution —
/// that's the native tool's job here, not archon's. archon's own
/// dependency graph resolver, AUR client, and sandboxed/eBPF-monitored
/// build pipeline stay Arch-specific; this trait exists for everything
/// that isn't Arch.
pub trait Backend: std::fmt::Debug {
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
    parts.iter().map(|s| s.to_string()).chain(names.iter().cloned()).collect()
}

#[derive(Debug, Clone, Copy)]
pub struct PacmanBackend;

impl Backend for PacmanBackend {
    fn name(&self) -> &'static str {
        "pacman"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["pacman", "-S", "--needed", "--noconfirm"] } else { &["pacman", "-S", "--needed"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["pacman", "-Rs", "--noconfirm"] } else { &["pacman", "-Rs"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["pacman".to_string(), "-Ss".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["pacman".to_string(), "-Q".to_string(), name.to_string()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AptBackend;

impl Backend for AptBackend {
    fn name(&self) -> &'static str {
        "apt"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["apt", "install", "-y"] } else { &["apt", "install"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["apt", "remove", "-y"] } else { &["apt", "remove"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["apt-cache".to_string(), "search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["dpkg".to_string(), "-s".to_string(), name.to_string()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DnfBackend;

impl Backend for DnfBackend {
    fn name(&self) -> &'static str {
        "dnf"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["dnf", "install", "-y"] } else { &["dnf", "install"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["dnf", "remove", "-y"] } else { &["dnf", "remove"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["dnf".to_string(), "search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["rpm".to_string(), "-q".to_string(), name.to_string()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ZypperBackend;

impl Backend for ZypperBackend {
    fn name(&self) -> &'static str {
        "zypper"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["zypper", "--non-interactive", "install"] } else { &["zypper", "install"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["zypper", "--non-interactive", "remove"] } else { &["zypper", "remove"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["zypper".to_string(), "search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        // openSUSE is RPM-based, same query tool as Fedora.
        vec!["rpm".to_string(), "-q".to_string(), name.to_string()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ApkBackend;

impl Backend for ApkBackend {
    fn name(&self) -> &'static str {
        "apk"
    }
    fn install_argv(&self, names: &[String], _assume_yes: bool) -> Vec<String> {
        // apk is non-interactive by design — there's no prompt for
        // `--yes` to skip, so the flag is accepted and simply moot here.
        argv(&["apk", "add"], names)
    }
    fn remove_argv(&self, names: &[String], _assume_yes: bool) -> Vec<String> {
        argv(&["apk", "del"], names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["apk".to_string(), "search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["apk".to_string(), "info".to_string(), "-e".to_string(), name.to_string()]
    }
}

/// NixOS. Uses the classic `nix-env` interface rather than the newer `nix
/// profile` commands deliberately: `nix profile` still requires the
/// `nix-command`/`flakes` experimental features to be enabled in nix.conf,
/// which a stock NixOS install doesn't guarantee, while `nix-env` works
/// everywhere. The `nixos.` attribute prefix is the channel name every
/// standard NixOS system has (this backend is only ever selected when
/// /etc/os-release says ID=nixos, so `nixpkgs.` — the prefix non-NixOS nix
/// installs use — would be the wrong guess here).
#[derive(Debug, Clone, Copy)]
pub struct NixBackend;

impl Backend for NixBackend {
    fn name(&self) -> &'static str {
        "nix-env"
    }
    fn install_argv(&self, names: &[String], _assume_yes: bool) -> Vec<String> {
        // nix-env never prompts, so assume_yes is moot (same as apk).
        let mut out = vec!["nix-env".to_string(), "-iA".to_string()];
        out.extend(names.iter().map(|n| format!("nixos.{n}")));
        out
    }
    fn remove_argv(&self, names: &[String], _assume_yes: bool) -> Vec<String> {
        // Removal goes by plain package name, not channel attribute.
        argv(&["nix-env", "-e"], names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["nix-env".to_string(), "-qaP".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["nix-env".to_string(), "-q".to_string(), name.to_string()]
    }
    /// Per-user profile — see the trait method's doc comment for why sudo
    /// here would be actively wrong, not just unnecessary.
    fn needs_root(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy)]
pub struct XbpsBackend;

impl Backend for XbpsBackend {
    fn name(&self) -> &'static str {
        "xbps"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["xbps-install", "-Sy"] } else { &["xbps-install", "-S"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        // -R removes now-orphaned dependencies too, matching what the
        // pacman backend's -Rs does on Arch.
        let base: &[&str] = if assume_yes { &["xbps-remove", "-Ry"] } else { &["xbps-remove", "-R"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["xbps-query".to_string(), "-Rs".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["xbps-query".to_string(), name.to_string()]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EmergeBackend;

impl Backend for EmergeBackend {
    fn name(&self) -> &'static str {
        "emerge"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        // emerge doesn't prompt by default; --ask *adds* the confirmation.
        // So archon's "no --yes given" maps to --ask (you get to review the
        // transaction) and --yes maps to plain emerge — inverted flag,
        // same user-visible semantics as every other backend.
        let base: &[&str] = if assume_yes { &["emerge"] } else { &["emerge", "--ask"] };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes { &["emerge", "--depclean"] } else { &["emerge", "--ask", "--depclean"] };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["emerge".to_string(), "--search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        // qlist ships in portage-utils, present on effectively every
        // Gentoo system (portage itself recommends it).
        vec!["qlist".to_string(), "-I".to_string(), name.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_install_argv() {
        assert_eq!(PacmanBackend.install_argv(&["firefox".into()], false), ["pacman", "-S", "--needed", "firefox"]);
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
        assert_eq!(AptBackend.install_argv(&["firefox".into()], true), ["apt", "install", "-y", "firefox"]);
    }

    #[test]
    fn dnf_remove_argv() {
        assert_eq!(DnfBackend.remove_argv(&["firefox".into()], true), ["dnf", "remove", "-y", "firefox"]);
    }

    #[test]
    fn dnf_remove_argv_without_assume_yes() {
        assert_eq!(DnfBackend.remove_argv(&["firefox".into()], false), ["dnf", "remove", "firefox"]);
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
        assert_eq!(AptBackend.search_argv("vim"), ["apt-cache", "search", "vim"]);
        assert_eq!(DnfBackend.search_argv("vim"), ["dnf", "search", "vim"]);
        assert_eq!(ZypperBackend.search_argv("vim"), ["zypper", "search", "vim"]);
        assert_eq!(ApkBackend.search_argv("vim"), ["apk", "search", "vim"]);
        assert_eq!(NixBackend.search_argv("vim"), ["nix-env", "-qaP", "vim"]);
        assert_eq!(XbpsBackend.search_argv("vim"), ["xbps-query", "-Rs", "vim"]);
        assert_eq!(EmergeBackend.search_argv("vim"), ["emerge", "--search", "vim"]);
    }

    #[test]
    fn nix_installs_by_channel_attribute_and_removes_by_name() {
        assert_eq!(
            NixBackend.install_argv(&["firefox".into(), "vim".into()], false),
            ["nix-env", "-iA", "nixos.firefox", "nixos.vim"]
        );
        assert_eq!(NixBackend.remove_argv(&["firefox".into()], false), ["nix-env", "-e", "firefox"]);
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
        assert_eq!(ZypperBackend.install_argv(&["vim".into()], false), ["zypper", "install", "vim"]);
    }

    #[test]
    fn apk_is_always_non_interactive() {
        // Same argv either way: apk has no prompt for --yes to skip.
        assert_eq!(ApkBackend.install_argv(&["vim".into()], false), ["apk", "add", "vim"]);
        assert_eq!(ApkBackend.install_argv(&["vim".into()], true), ["apk", "add", "vim"]);
        assert_eq!(ApkBackend.remove_argv(&["vim".into()], true), ["apk", "del", "vim"]);
    }

    #[test]
    fn xbps_argv() {
        assert_eq!(XbpsBackend.install_argv(&["vim".into()], false), ["xbps-install", "-S", "vim"]);
        assert_eq!(XbpsBackend.install_argv(&["vim".into()], true), ["xbps-install", "-Sy", "vim"]);
        assert_eq!(XbpsBackend.remove_argv(&["vim".into()], true), ["xbps-remove", "-Ry", "vim"]);
    }

    #[test]
    fn emerge_ask_flag_is_inverted_but_semantics_match() {
        // No --yes: user still gets a confirmation (emerge's own --ask).
        assert_eq!(EmergeBackend.install_argv(&["vim".into()], false), ["emerge", "--ask", "vim"]);
        // --yes: no prompt anywhere, same as -y elsewhere.
        assert_eq!(EmergeBackend.install_argv(&["vim".into()], true), ["emerge", "vim"]);
    }
}
