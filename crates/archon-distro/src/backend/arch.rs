use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct PacmanBackend;

impl Backend for PacmanBackend {
    fn operation_unavailable_reason(&self, operation: Operation) -> Option<&'static str> {
        (operation == Operation::Refresh).then_some("standalone refresh is unavailable on Arch; use `archon upgrade` to refresh and upgrade together")
    }

    fn operation_needs_root(&self, operation: Operation) -> bool {
        operation.mutates() || operation == Operation::Check
    }

    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let base: &[&str] = match operation {
            Operation::Upgrade => &["pacman", "-Syu"],
            // Refreshing Arch metadata alone can lead to partial upgrades.
            Operation::Refresh => return None,
            Operation::Info => &["pacman", "-Si"],
            Operation::List => &["pacman", "-Q"],
            Operation::Reinstall => &["pacman", "-S"],
            Operation::Check => &["pacman", "-Dk"],
            _ => return None,
        };
        let mut out = argv(base, &[]);
        if assume_yes && matches!(operation, Operation::Upgrade | Operation::Reinstall) {
            out.push("--noconfirm".into());
        }
        out.extend_from_slice(names);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "pacman"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["pacman", "-S", "--needed", "--noconfirm"]
        } else {
            &["pacman", "-S", "--needed"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["pacman", "-Rs", "--noconfirm"]
        } else {
            &["pacman", "-Rs"]
        };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["pacman".to_string(), "-Ss".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["pacman".to_string(), "-Q".to_string(), name.to_string()]
    }
}
