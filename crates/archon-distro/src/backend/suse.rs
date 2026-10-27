use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct ZypperBackend;

impl Backend for ZypperBackend {
    fn inventory_argv(&self) -> Option<Vec<String>> {
        Some(super::rpm::inventory_argv())
    }

    fn parse_inventory(&self, output: &str) -> Result<Vec<super::InstalledPackage>, String> {
        super::rpm::parse_inventory(output)
    }

    fn operation_argv(
        &self,
        operation: Operation,
        names: &[String],
        assume_yes: bool,
    ) -> Option<Vec<String>> {
        let base: &[&str] = match operation {
            Operation::Upgrade => &["update"],
            Operation::Refresh => &["refresh"],
            Operation::Info => &["info"],
            Operation::List => return Some(argv(&["rpm", "-qa"], names)),
            Operation::Reinstall => &["install", "--force"],
            Operation::Repositories => &["repos"],
            Operation::Provides => &["what-provides"],
            _ => return None,
        };
        let mut out = vec!["zypper".into()];
        if assume_yes && operation.mutates() {
            out.push("--non-interactive".into());
        }
        out.extend(base.iter().map(|s| s.to_string()));
        out.extend_from_slice(names);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "zypper"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["zypper", "--non-interactive", "install"]
        } else {
            &["zypper", "install"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["zypper", "--non-interactive", "remove"]
        } else {
            &["zypper", "remove"]
        };
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
