use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct AptBackend;

impl Backend for AptBackend {
    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let base: &[&str] = match operation {
            Operation::Upgrade => &["apt", "upgrade"],
            Operation::Refresh => &["apt", "update"],
            Operation::Info => &["apt-cache", "show"],
            Operation::List => &["apt", "list", "--installed"],
            Operation::Reinstall => &["apt", "reinstall"],
            Operation::Repositories => &["apt-cache", "policy"],
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
        "apt"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["apt", "install", "-y"]
        } else {
            &["apt", "install"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["apt", "remove", "-y"]
        } else {
            &["apt", "remove"]
        };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec![
            "apt-cache".to_string(),
            "search".to_string(),
            term.to_string(),
        ]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["dpkg".to_string(), "-s".to_string(), name.to_string()]
    }
}
