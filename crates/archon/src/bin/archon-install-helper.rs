//! Administrator-installed receiver. No paths/commands are accepted as argv.
#![allow(dead_code)] // Shared protocol module also exposes the client setup path.
#[path = "../handoff.rs"]
mod handoff;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use std::sync::atomic::Ordering;

extern "C" fn interrupt(_: libc::c_int) {
    handoff::INTERRUPTED.store(true, Ordering::Relaxed);
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "--version") {
        println!(
            "archon-install-helper {}\nReceives bounded package data on stdin; administrator use only.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    anyhow::ensure!(
        args.is_empty(),
        "helper accepts package data on stdin, never paths or options"
    );
    anyhow::ensure!(
        unsafe { libc::geteuid() } == 0 && unsafe { libc::getuid() } == 0,
        "helper must run through administrator elevation; never install it setuid"
    );
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = interrupt as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let base = handoff::prepare_root()?;
    let transaction = handoff::StagedTransaction::receive(&base, &mut std::io::stdin().lock())?;
    let terminal = File::open("/dev/tty")
        .context("cannot open the controlling terminal for pacman's confirmation prompt")?;
    let mut child = pacman_command(
        Path::new("/usr/bin/pacman"),
        transaction.packages(),
        terminal,
    )
    .spawn()?;
    let mut forwarded = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if handoff::INTERRUPTED.load(Ordering::Relaxed) && !forwarded {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            forwarded = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    anyhow::ensure!(status.success(), "pacman failed with {status}");
    anyhow::ensure!(!forwarded, "installation interrupted");
    Ok(())
}

fn pacman_command(pacman: &Path, packages: &[PathBuf], terminal: File) -> Command {
    let mut command = Command::new(pacman);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", "/root")
        .args(["--config", "/etc/pacman.conf", "-U", "--needed", "--"])
        .args(packages)
        // Helper stdin carries the package transaction; pacman must read its
        // confirmation response from the caller's controlling terminal.
        .stdin(Stdio::from(terminal))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn pacman_confirmation_reads_from_separate_input_stream() {
        let dir = tempfile::tempdir().unwrap();
        let pacman = dir.path().join("pacman-mock");
        std::fs::write(
            &pacman,
            "#!/bin/sh\nread answer || exit 17\n[ \"$answer\" = y ]\n",
        )
        .unwrap();
        std::fs::set_permissions(&pacman, std::fs::Permissions::from_mode(0o755)).unwrap();
        let response = dir.path().join("terminal-input");
        std::fs::write(&response, b"y\n").unwrap();
        let terminal = File::open(response).unwrap();

        let status = pacman_command(&pacman, &[], terminal).status().unwrap();
        assert!(status.success(), "mock pacman exited with {status}");
    }
}
