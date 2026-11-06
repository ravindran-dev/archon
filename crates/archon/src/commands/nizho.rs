//! Nizho routes package operations to its authorized system service.
//! Existing native distro command implementations are untouched.
use crate::Command;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
pub fn dispatch(command: &Command) -> Option<Result<()>> {
    if !matches!(archon_distro::Distro::detect(), archon_distro::Distro::Unknown(name) if name.starts_with("Nizho"))
    {
        return None;
    }
    let request: Value = match command {
        Command::Search { term, all } => json!({"command":"search","query":term,"all":all}),
        Command::List { dry_run: false, .. } => json!({"command":"installed"}),
        Command::Repos(options) if !options.dry_run => json!({"command":"repositories"}),
        Command::History(options) if !options.dry_run => json!({"command":"history"}),
        Command::Capabilities => json!({"command":"capabilities"}),
        Command::Info { packages, options } if !options.dry_run => {
            json!({"command":"info","packages":packages})
        }
        Command::Install {
            packages,
            dry_run,
            yes,
            refresh: false,
            report: false,
        } => json!({"command":"install","packages":packages,"dry_run":dry_run,"yes":yes}),
        Command::Remove {
            packages,
            dry_run,
            yes,
        } => json!({"command":"remove","packages":packages,"dry_run":dry_run,"yes":yes}),
        Command::Plan {
            packages,
            native: false,
            ..
        } => json!({"command":"install","packages":packages,"dry_run":true,"yes":false}),
        Command::Upgrade(options) => {
            json!({"command":"update","dry_run":options.dry_run,"yes":options.yes})
        }
        // Never fall through to a live native mutation on an immutable host.
        _ => {
            return Some(Err(anyhow::anyhow!(
                "This operation is not supported by Nizho's immutable package backend. Use Capsule for system image status."
            )));
        }
    };
    Some(run(&request))
}
fn run(request: &Value) -> Result<()> {
    let status = std::process::Command::new("/usr/bin/python3")
        .args([
            "-I",
            "-c",
            include_str!("nizho_client.py"),
            &request.to_string(),
        ])
        .status()
        .context("starting Nizho's unprivileged D-Bus client")?;
    if !status.success() {
        bail!(
            "Nizho package service operation failed (exit {:?})",
            status.code()
        );
    }
    Ok(())
}
