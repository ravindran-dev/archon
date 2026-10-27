use super::{Backend, Operation, argv};

#[derive(Debug, Clone, Copy)]
pub struct DnfBackend;

impl Backend for DnfBackend {
    fn transaction_engine(&self) -> Option<&'static str> {
        Some("libdnf5")
    }

    fn transaction_bridge(&self) -> Option<&'static str> {
        Some(crate::transaction::FEDORA_BRIDGE)
    }

    fn mutation_restriction(&self, ostree_booted: bool) -> Option<&'static str> {
        ostree_booted.then_some("rpm-ostree deployment detected; the DNF adapter cannot modify this host. An rpm-ostree adapter is required")
    }

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
        // This subset is shared by DNF4 and DNF5, including repolist's
        // compatibility alias. No dependency on the Arch resolver or AUR.
        let base: &[&str] = match operation {
            Operation::Upgrade => &["dnf", "--refresh", "upgrade"],
            Operation::Refresh => &["dnf", "--refresh", "makecache"],
            Operation::Info => &["dnf", "info"],
            Operation::List => &["rpm", "-qa"],
            Operation::Reinstall => &["dnf", "reinstall"],
            Operation::Repositories => &["dnf", "repolist", "--all"],
            Operation::Check => &["dnf", "check"],
            Operation::History => &["dnf", "history", "list"],
            Operation::Provides => &["dnf", "provides"],
        };
        let mut out = argv(base, &[]);
        if assume_yes && matches!(operation, Operation::Upgrade | Operation::Reinstall) {
            out.push("-y".into());
        }
        out.extend_from_slice(names);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "dnf"
    }
    fn install_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["dnf", "install", "-y"]
        } else {
            &["dnf", "install"]
        };
        argv(base, names)
    }
    fn remove_argv(&self, names: &[String], assume_yes: bool) -> Vec<String> {
        let base: &[&str] = if assume_yes {
            &["dnf", "remove", "-y"]
        } else {
            &["dnf", "remove"]
        };
        argv(base, names)
    }
    fn search_argv(&self, term: &str) -> Vec<String> {
        vec!["dnf".to_string(), "search".to_string(), term.to_string()]
    }
    fn is_installed_argv(&self, name: &str) -> Vec<String> {
        vec!["rpm".to_string(), "-q".to_string(), name.to_string()]
    }
}
