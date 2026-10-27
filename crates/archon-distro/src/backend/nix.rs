use super::{Backend, Operation, argv};

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
    fn scope(&self) -> &'static str {
        "current user profile/channels; declarative NixOS configuration is managed separately"
    }

    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let _ = assume_yes; // Profile operations do not prompt.
        let base: &[&str] = match operation {
            Operation::Upgrade => &["nix-env", "--upgrade"],
            Operation::Refresh => &["nix-channel", "--update"],
            Operation::Info => &["nix-env", "-qa", "--description"],
            Operation::List => &["nix-env", "-q"],
            Operation::Repositories => &["nix-channel", "--list"],
            _ => return None,
        };
        Some(argv(base, names))
    }

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
