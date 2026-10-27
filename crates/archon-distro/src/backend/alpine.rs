use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct ApkBackend;

impl Backend for ApkBackend {
    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let base: &[&str] = match operation {
            Operation::Upgrade => &["apk", "upgrade", "--update-cache"],
            Operation::Refresh => &["apk", "update"],
            Operation::Info => &["apk", "info", "--all"],
            Operation::List => &["apk", "info", "-v"],
            Operation::Reinstall => &["apk", "fix", "--reinstall"],
            _ => return None,
        };
        let _ = assume_yes; // apk's existing noninteractive behavior.
        Some(argv(base, names))
    }

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
        vec![
            "apk".to_string(),
            "info".to_string(),
            "-e".to_string(),
            name.to_string(),
        ]
    }
}
