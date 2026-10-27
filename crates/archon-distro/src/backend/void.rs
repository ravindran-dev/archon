use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct XbpsBackend;

impl Backend for XbpsBackend {
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
            Operation::Upgrade => &["xbps-install", "-Su"],
            Operation::Refresh => &["xbps-install", "-S"],
            Operation::Info => &["xbps-query", "-R"],
            Operation::List => &["xbps-query", "-l"],
            Operation::Reinstall => &["xbps-install", "-f"],
            Operation::Repositories => &["xbps-query", "-L"],
            Operation::Check => &["xbps-pkgdb", "-a"],
            _ => return None,
        };
        let mut out = argv(base, &[]);
        if assume_yes && matches!(operation, Operation::Upgrade | Operation::Reinstall) {
            out.push("-y".into());
        }
        out.extend_from_slice(names);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "xbps"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["xbps-install", "-Sy"]
        } else {
            &["xbps-install", "-S"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        // -R removes now-orphaned dependencies too, matching what the
        // pacman backend's -Rs does on Arch.
        let base: &[&str] = if assume_yes {
            &["xbps-remove", "-Ry"]
        } else {
            &["xbps-remove", "-R"]
        };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec![
            "xbps-query".to_string(),
            "-Rs".to_string(),
            term.to_string(),
        ]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["xbps-query".to_string(), name.to_string()]
    }
}
