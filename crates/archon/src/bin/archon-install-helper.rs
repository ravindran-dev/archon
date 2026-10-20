//! Administrator-installed receiver. No paths/commands are accepted as argv.
#![allow(dead_code)] // Shared protocol module also exposes the client setup path.
#[path = "../handoff.rs"]
mod handoff;

use std::process::Command;
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
    let mut child = Command::new("/usr/bin/pacman")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", "/root")
        .args(["--config", "/etc/pacman.conf", "-U", "--needed", "--"])
        .args(transaction.packages())
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
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
