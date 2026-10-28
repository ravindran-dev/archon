//! Shared dispatch knows capabilities and privilege requirements, never distro argv.
use std::process::Command;

use anyhow::{Context, Result, bail};
use archon_distro::{Backend, Distro, Operation};

use super::install::validate_package_names;
use crate::ui;

pub fn run(operation: Operation, packages: &[String], dry_run: bool, yes: bool) -> Result<()> {
    validate_package_names(packages)?;
    if operation.requires_packages() && packages.is_empty() {
        bail!("{} requires at least one package", operation.name());
    }
    if !operation.requires_packages() && !packages.is_empty() {
        bail!("{} does not accept package targets", operation.name());
    }
    let distro = Distro::detect();
    let backend = backend(&distro)?;
    let Some(argv) = backend.operation_argv(operation, packages, yes) else {
        if let Some(reason) = backend.operation_unavailable_reason(operation) {
            bail!("{reason}");
        }
        bail!(
            "{} is not supported by the {} adapter; run `archon capabilities`",
            operation.name(),
            backend.name()
        );
    };
    if operation.mutates() {
        ensure_mutable_host(&distro)?;
    }
    println!(
        "Detected {distro} — {} via {}",
        operation.name(),
        backend.name()
    );
    println!("Scope: {}", backend.scope());
    let root = backend.operation_needs_root(operation) && unsafe { libc::geteuid() } != 0;
    println!("{}{}", if root { "sudo " } else { "" }, display_argv(&argv));
    if dry_run {
        println!("Command preview only; no native transaction was resolved or executed.");
        return Ok(());
    }
    if operation.mutates() && !yes && !ui::confirm("Proceed with this operation?")? {
        println!("aborted — nothing was changed.");
        return Ok(());
    }
    let (program, args) = argv
        .split_first()
        .context("adapter returned an empty command")?;
    let mut command = if root {
        let mut command = Command::new("sudo");
        command.args(["--", program]);
        command
    } else {
        Command::new(program)
    };
    let status = command.args(args).status().with_context(|| {
        format!(
            "cannot execute {program}; install the native tool required by the {} adapter",
            backend.name()
        )
    })?;
    if !status.success() {
        bail!(
            "{} {} failed with {status}",
            backend.name(),
            operation.name()
        );
    }
    Ok(())
}

pub fn inventory() -> Result<()> {
    let distro = Distro::detect();
    let backend = backend(&distro)?;
    let argv = backend.inventory_argv().with_context(|| {
        format!(
            "structured inventory is not available for {}; use `archon list` for native output",
            backend.name()
        )
    })?;
    let (program, args) = argv.split_first().context("empty inventory command")?;
    // Never escalate a read-only inventory query; stdout is JSON only.
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("cannot execute {program}"))?;
    if !output.status.success() {
        bail!(
            "inventory query failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let records = backend
        .parse_inventory(std::str::from_utf8(&output.stdout)?)
        .map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}

pub fn capabilities() -> Result<()> {
    let distro = Distro::detect();
    if distro.to_string().starts_with("ArkaOS") {
        println!(
            "{}",
            serde_json::json!({"backend":"arkaos-image", "live_mutations":"unsupported", "image_planning":"unsupported", "service":"org.arka.Archon1"})
        );
        return Ok(());
    }
    let backend = backend(&distro)?;
    println!("Distro: {distro}\nBackend: {}", backend.name());
    println!("Scope: {}", backend.scope());
    println!("install/remove/search: supported");
    println!(
        "prepared_transactions: {}",
        backend.transaction_engine().unwrap_or("unavailable")
    );
    for operation in Operation::ALL {
        let supported = backend.operation_argv(operation, &[], false).is_some();
        println!(
            "{}: {}",
            operation.name(),
            if supported {
                "supported"
            } else {
                "unavailable"
            }
        );
    }
    println!(
        "list --json: {}",
        if backend.inventory_argv().is_some() {
            "supported"
        } else {
            "unavailable"
        }
    );
    if let Err(error) = ensure_mutable_host(&distro) {
        println!("Host restriction: {error}");
    }
    println!(
        "Capabilities describe implemented operations; native tools and repository access are required."
    );
    Ok(())
}

fn backend(distro: &Distro) -> Result<Box<dyn Backend>> {
    distro
        .backend()
        .with_context(|| format!("no supported package manager detected for {distro}"))
}

/// Fedora family IDs also occur on rpm-ostree hosts. Do not run a DNF
/// mutation on an image-based deployment just because ID says Fedora.
pub(crate) fn ensure_mutable_host(distro: &Distro) -> Result<()> {
    validate_deployment(distro, std::path::Path::new("/run/ostree-booted").exists())
}

fn validate_deployment(distro: &Distro, ostree_booted: bool) -> Result<()> {
    let backend = backend(distro)?;
    if let Some(reason) = backend.mutation_restriction(ostree_booted) {
        bail!("{reason}");
    }
    Ok(())
}

fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/_-.@=:+".contains(&c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ostree_restriction_is_confined_to_the_fedora_adapter() {
        assert!(validate_deployment(&Distro::Fedora, true).is_err());
        assert!(validate_deployment(&Distro::Fedora, false).is_ok());
        assert!(validate_deployment(&Distro::Arch, true).is_ok());
        assert!(validate_deployment(&Distro::NixOs, true).is_ok());
    }

    #[test]
    fn displayed_commands_preserve_argument_boundaries() {
        assert_eq!(
            display_argv(&["dnf".into(), "provides".into(), "*/it's a lib".into()]),
            "dnf provides '*/it'\\''s a lib'"
        );
    }
}
