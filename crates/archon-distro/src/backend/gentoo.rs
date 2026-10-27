use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct EmergeBackend;

impl Backend for EmergeBackend {
    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let base: &[&str] = match operation {
            Operation::Upgrade => &["emerge", "--update", "--deep", "--newuse", "@world"],
            Operation::Refresh => &["emerge", "--sync"],
            Operation::Info => &["emerge", "--info"],
            // qlist is an optional portage-utils dependency, checked at execution.
            Operation::List => &["qlist", "-Iv"],
            Operation::Reinstall => &["emerge"],
            Operation::Repositories => &["portageq", "get_repos", "/"],
            _ => return None,
        };
        let mut out = argv(base, &[]);
        if !assume_yes && matches!(operation, Operation::Upgrade | Operation::Reinstall) {
            out.push("--ask".into());
        }
        out.extend_from_slice(names);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "emerge"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        // emerge doesn't prompt by default; --ask *adds* the confirmation.
        // So archon's "no --yes given" maps to --ask (you get to review the
        // transaction) and --yes maps to plain emerge — inverted flag,
        // same user-visible semantics as every other backend.
        let base: &[&str] = if assume_yes {
            &["emerge"]
        } else {
            &["emerge", "--ask"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["emerge", "--depclean"]
        } else {
            &["emerge", "--ask", "--depclean"]
        };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec![
            "emerge".to_string(),
            "--search".to_string(),
            term.to_string(),
        ]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        // qlist ships in portage-utils, present on effectively every
        // Gentoo system (portage itself recommends it).
        vec!["qlist".to_string(), "-I".to_string(), name.to_string()]
    }
}
