//! Update Archon itself from the project's GitHub main branch.
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const REPOSITORY: &str = "https://github.com/ravindran-dev/archon.git";

fn installation_root(executable: &Path) -> Result<PathBuf> {
    let bin = executable
        .parent()
        .context("cannot locate executable directory")?;
    if bin.file_name().is_none_or(|name| name != "bin") {
        bail!(
            "cannot infer the install root from {}; specify --root (for example --root ~/.local)",
            executable.display()
        );
    }
    Ok(bin
        .parent()
        .context("cannot locate installation root")?
        .to_path_buf())
}

pub fn run(root: Option<PathBuf>, dry_run: bool, yes: bool) -> Result<()> {
    let root = match root {
        Some(root) => root,
        None => installation_root(&std::env::current_exe()?)?,
    };
    println!("Update Archon and its companion helper from {REPOSITORY} (main).");
    println!("Install directory: {}", root.join("bin").display());
    println!(
        "cargo install --git {REPOSITORY} --branch main --locked --force --bins --root {root:?} archon-cli"
    );
    if dry_run {
        return Ok(());
    }
    if unsafe { libc::geteuid() } == 0 {
        bail!(
            "run self-update as your normal user, without sudo; use --root ~/.local for a user installation"
        );
    }
    if !yes && !crate::ui::confirm("Build and install the latest Archon from GitHub?")? {
        println!("cancelled — Archon was not updated.");
        return Ok(());
    }
    // Let Cargo fetch main and build before replacing the installed binaries.
    // Inherit its output so Git downloads and compilation remain visible.
    let status = Command::new("cargo")
        .args([
            "install", "--git", REPOSITORY, "--branch", "main", "--locked", "--force", "--bins",
            "--root",
        ])
        .arg(&root)
        .arg("archon-cli")
        .status()
        .context("could not run Cargo; install the Rust toolchain and ensure cargo is in PATH")?;
    if !status.success() {
        bail!(
            "Archon self-update failed ({status}); inspect Cargo's output above and retry after resolving the error"
        );
    }
    println!(
        "Archon updated in {}. Run `archon --help` to see available commands.",
        root.join("bin").display()
    );
    println!(
        "If you use the privileged AUR installation helper, run `archon setup-helper` to refresh its trusted copy."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_the_executables_actual_installation_root() {
        assert_eq!(
            installation_root(Path::new("/home/user/.local/bin/archon")).unwrap(),
            Path::new("/home/user/.local")
        );
        assert_eq!(
            installation_root(Path::new("/home/user/.cargo/bin/archon")).unwrap(),
            Path::new("/home/user/.cargo")
        );
        assert!(installation_root(Path::new("/tmp/build/archon")).is_err());
    }
}
