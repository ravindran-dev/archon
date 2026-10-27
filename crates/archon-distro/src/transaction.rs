//! Versioned, native-engine-independent transaction protocol.
//! Native versions are opaque strings; adapters own their comparison semantics.
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const FEDORA_BRIDGE: &str = include_str!("fedora_transaction.py");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOperation {
    Install,
    Remove,
    Upgrade,
    Reinstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRequest {
    pub operation: TransactionOperation,
    pub packages: Vec<String>,
}

impl TransactionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.operation == TransactionOperation::Upgrade {
            if !self.packages.is_empty() {
                return Err(
                    "upgrade applies to all installed packages; package targets are not accepted"
                        .into(),
                );
            }
        } else if self.packages.is_empty() {
            return Err("this operation requires package targets".into());
        }
        if self.packages.len() > 256
            || self.packages.iter().any(|name| {
                name.is_empty()
                    || name.len() > 256
                    || !name.as_bytes()[0].is_ascii_alphanumeric()
                    || !name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"+_.:@<>=~-".contains(&c))
            })
        {
            return Err("invalid package target; use native package names or version specs, without options, paths or groups".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub name: String,
    pub version: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeAction {
    Install,
    Remove,
    Upgrade,
    Downgrade,
    Reinstall,
    Replaced,
    ReasonChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageChange {
    pub action: ChangeAction,
    pub package: PackageIdentity,
    pub reason: String,
    pub repository: String,
    pub download_bytes: u64,
    pub install_bytes: u64,
    pub checksum: Option<String>,
    pub replaces: Vec<PackageIdentity>,
    pub replaced_by: Vec<PackageIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionPlan {
    pub schema_version: u32,
    pub backend: String,
    pub request: TransactionRequest,
    pub changes: Vec<PackageChange>,
    /// Total archive sizes; cache reuse may reduce the actual network transfer.
    pub download_bytes: u64,
    /// Conservative sum of inbound installed sizes, not net filesystem growth.
    pub install_bytes: u64,
    pub system_fingerprint: String,
    pub policy_fingerprint: String,
    pub warnings: Vec<String>,
}

impl TransactionPlan {
    pub fn validate(&self) -> Result<(), String> {
        self.request.validate()?;
        if self.schema_version != SCHEMA_VERSION
            || self.backend.is_empty()
            || !self
                .backend
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("unsupported transaction schema or engine".into());
        }
        if [&self.system_fingerprint, &self.policy_fingerprint]
            .iter()
            .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("missing or invalid native state fingerprint".into());
        }
        let download = self
            .changes
            .iter()
            .try_fold(0u64, |sum, item| sum.checked_add(item.download_bytes));
        let installed = self
            .changes
            .iter()
            .try_fold(0u64, |sum, item| sum.checked_add(item.install_bytes));
        if download != Some(self.download_bytes) || installed != Some(self.install_bytes) {
            return Err("inconsistent transaction size totals".into());
        }
        for change in &self.changes {
            for identity in std::iter::once(&change.package)
                .chain(&change.replaces)
                .chain(&change.replaced_by)
            {
                if [&identity.name, &identity.version, &identity.architecture]
                    .iter()
                    .any(|value| value.is_empty() || value.chars().any(char::is_control))
                {
                    return Err("invalid native package identity".into());
                }
            }
            let inbound = matches!(
                change.action,
                ChangeAction::Install
                    | ChangeAction::Upgrade
                    | ChangeAction::Downgrade
                    | ChangeAction::Reinstall
            );
            if inbound
                && change
                    .checksum
                    .as_ref()
                    .is_none_or(|value| !value.contains(':') || value.ends_with(':'))
            {
                return Err("inbound package is missing its native repository checksum".into());
            }
            if !inbound && (change.download_bytes != 0 || change.install_bytes != 0) {
                return Err(
                    "outbound packages cannot contribute download or inbound installed bytes"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_cannot_be_native_options_or_configuration_paths() {
        for target in [
            "",
            "--nogpgcheck",
            "/tmp/pkg.rpm",
            "@group",
            "a\nb",
            "$(id)",
        ] {
            assert!(
                TransactionRequest {
                    operation: TransactionOperation::Install,
                    packages: vec![target.into()]
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            TransactionRequest {
                operation: TransactionOperation::Install,
                packages: vec!["bash>=5.2".into()]
            }
            .validate()
            .is_ok()
        );
        assert!(
            TransactionRequest {
                operation: TransactionOperation::Upgrade,
                packages: vec!["bash".into()]
            }
            .validate()
            .is_err()
        );
    }
}
