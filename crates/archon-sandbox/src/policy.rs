use std::path::PathBuf;

/// Bubblewrap invocation policy for one build stage.
///
/// Every stage shares the same shape: the host root is bound read-only so
/// the toolchain (gcc, make, pacman's db for `makepkg`'s dependency
/// sanity-check, ...) stays visible, and only `pkg_dir` — the AUR
/// package's clone directory, which becomes the parent of `$srcdir` and
/// `$pkgdir` once `makepkg` runs — is writable. `/home` and `/root` are
/// masked with tmpfs *before* `pkg_dir` is rebound on top, so a build
/// script can't read other users' files even though the rest of `/` is
/// visible; if `pkg_dir` happens to live outside `/home` this mask simply
/// doesn't apply to it.
///
/// User, PID, IPC, UTS, and cgroup namespaces are always unshared; network
/// is the only namespace toggled per stage, via `allow_network`.
pub struct SandboxPolicy {
    pub pkg_dir: PathBuf,
    pub allow_network: bool,
    pub extra_env: Vec<(String, String)>,
}

impl SandboxPolicy {
    pub fn new(pkg_dir: impl Into<PathBuf>, allow_network: bool) -> Self {
        Self {
            pkg_dir: pkg_dir.into(),
            allow_network,
            extra_env: Vec::new(),
        }
    }

    /// Build the full `bwrap` argument list ending in `--` plus `command`.
    pub fn bwrap_args(&self, command: &[String]) -> Vec<String> {
        let pkg = self.pkg_dir.to_string_lossy().into_owned();
        let mut args: Vec<String> = vec![
            "--die-with-parent".into(),
            "--new-session".into(),
            "--unshare-user".into(),
            "--unshare-pid".into(),
            "--unshare-ipc".into(),
            "--unshare-uts".into(),
            // Deliberately NOT --unshare-cgroup: cgroup namespace isolation
            // isn't load-bearing for this sandbox's actual security
            // properties (those come from the mount/user/net namespaces
            // above), and on a cgroup v2 host with the `nsdelegate` mount
            // option (confirmed present via `mount | grep cgroup` during
            // Phase 3 live testing), bwrap's cgroup-v2 delegation logic
            // reacts to --unshare-cgroup by creating its own private
            // sub-cgroup and moving the sandboxed process into that — a
            // *different* cgroup ID than the one archon-monitor's eBPF
            // program was told to watch, so every event went unobserved.
            // Omitting the flag keeps the process in exactly the cgroup
            // archon placed it in.
        ];
        if !self.allow_network {
            args.push("--unshare-net".into());
        }
        // Bind order matters: bwrap applies mounts in sequence, and each
        // later mount overrides earlier ones at the same path. The whole
        // host must be bound *first* so it forms the base layer; the
        // virtual /proc, /dev, /tmp mounts are then layered on top of it
        // (reversing this would let the plain host /dev — bound read-only,
        // where writes to /dev/null then fail with EPERM — silently
        // shadow our sandboxed devtmpfs instead of the other way around).
        args.extend(["--cap-drop".into(), "ALL".into()]);
        // Whole host visible read-only: toolchain, headers, pacman db.
        args.extend(["--ro-bind".into(), "/".into(), "/".into()]);
        args.extend(
            [
                "--proc", "/proc",
                "--dev", "/dev",
                "--tmpfs", "/tmp",
            ]
            .map(String::from),
        );
        // Mask other users' homes and root's home before re-exposing just
        // the build directory, in case it happens to live under /home.
        args.extend(["--tmpfs".into(), "/home".into()]);
        args.extend(["--tmpfs".into(), "/root".into()]);
        // Mask /run too: it isn't a home directory, but GnuPG's agent and
        // keyboxd sockets live under /run/user/<uid>/gnupg/ regardless of
        // $HOME, and that path was still the *host's* real, unmasked /run
        // — so a sandboxed `gpg` silently talked to the real user's
        // already-running agent instead of anything scoped to the build
        // directory (confirmed live: `gpg --list-keys` inside the sandbox
        // showed the real user's actual imported keys from unrelated past
        // builds, not an empty keyring). Masking /run forces gpg to spin
        // up its own agent rooted at $HOME/.gnupg like the isolation was
        // always meant to imply. /etc/resolv.conf on this system is a
        // real file rather than a /run symlink, so this doesn't cost DNS
        // resolution during the networked Fetch stage.
        args.extend(["--tmpfs".into(), "/run".into()]);
        args.extend(["--bind".into(), pkg.clone(), pkg.clone()]);
        args.extend(["--chdir".into(), pkg.clone()]);
        args.push("--clearenv".into());
        for (key, value) in self.base_env() {
            args.push("--setenv".into());
            args.push(key);
            args.push(value);
        }
        for (key, value) in &self.extra_env {
            args.push("--setenv".into());
            args.push(key.clone());
            args.push(value.clone());
        }
        args.push("--".into());
        args.extend(command.iter().cloned());
        args
    }

    /// Minimal, hygienic environment: HOME points inside the writable
    /// package dir so incidental dotfile writes (build caches, etc.) land
    /// in the sandbox instead of failing against the read-only root.
    fn base_env(&self) -> Vec<(String, String)> {
        let pkg = self.pkg_dir.to_string_lossy().into_owned();
        vec![
            ("HOME".into(), pkg),
            (
                "PATH".into(),
                "/usr/local/sbin:/usr/local/bin:/usr/bin".into(),
            ),
            (
                "TERM".into(),
                std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
            ),
            (
                "LANG".into(),
                std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
            ),
            // makepkg's package() step chowns everything to 0:0 under
            // fakeroot (every pacman package is root-owned). On a plain
            // host that real chown() gets EPERM, which fakeroot swallows
            // and fakes; but --unshare-user's uid_map only maps the real
            // caller's own uid (confirmed live via `bwrap --unshare-user --
            // id`), so uid 0 is an *unmapped* target and the same real
            // chown() gets EINVAL instead - a kernel-level distinction
            // fakeroot doesn't treat as swallowable, so it propagated the
            // failure straight to `cp`/`chown` and aborted every build with
            // enough files to touch a root-owned path (confirmed: brave-bin
            // package() failed this way). Telling fakeroot to skip the real
            // syscall entirely and just fake the state in its own database
            // sidesteps the errno distinction altogether - confirmed fixed
            // live with `FAKEROOTDONTTRYCHOWN=1` inside an identical
            // unshare -U sandbox.
            ("FAKEROOTDONTTRYCHOWN".into(), "1".into()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_stage_omits_unshare_net() {
        let policy = SandboxPolicy::new("/tmp/pkg", true);
        let args = policy.bwrap_args(&["makepkg".into()]);
        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn build_stage_unshares_net() {
        let policy = SandboxPolicy::new("/tmp/pkg", false);
        let args = policy.bwrap_args(&["makepkg".into()]);
        assert!(args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn pkg_dir_bound_writable_after_masking() {
        let policy = SandboxPolicy::new("/home/user/build/foo", false);
        let args = policy.bwrap_args(&["makepkg".into()]);
        let home_mask = args.iter().position(|a| a == "/home").unwrap();
        let pkg_bind = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == "/home/user/build/foo")
            .unwrap();
        assert!(pkg_bind > home_mask, "pkg_dir bind must come after the /home mask");
    }

    #[test]
    fn command_appears_after_separator() {
        let policy = SandboxPolicy::new("/tmp/pkg", true);
        let args = policy.bwrap_args(&["makepkg".into(), "--nobuild".into()]);
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[sep + 1..], &["makepkg", "--nobuild"]);
    }

    #[test]
    fn root_is_read_only() {
        let policy = SandboxPolicy::new("/tmp/pkg", true);
        let args = policy.bwrap_args(&["makepkg".into()]);
        let pos = args.iter().position(|a| a == "--ro-bind").unwrap();
        assert_eq!(args[pos + 1], "/");
        assert_eq!(args[pos + 2], "/");
    }

    /// Regression test: `--ro-bind / /` must precede `--dev`/`--proc`/
    /// `--tmpfs /tmp`. bwrap applies mounts in argument order and each
    /// later mount overrides earlier ones at the same path — getting this
    /// backwards once meant the read-only host /dev silently replaced the
    /// sandboxed devtmpfs, and writes to /dev/null failed with EPERM
    /// (which in turn made makepkg misreport an unrelated BUILDDIR error).
    #[test]
    fn root_bind_precedes_virtual_mounts() {
        let policy = SandboxPolicy::new("/tmp/pkg", true);
        let args = policy.bwrap_args(&["makepkg".into()]);
        let ro_bind_pos = args.iter().position(|a| a == "--ro-bind").unwrap();
        for virtual_mount in ["--dev", "--proc"] {
            let pos = args
                .iter()
                .position(|a| a == virtual_mount)
                .unwrap_or_else(|| panic!("missing {virtual_mount}"));
            assert!(
                pos > ro_bind_pos,
                "{virtual_mount} must come after --ro-bind / / so it overrides the host mount"
            );
        }
        let tmp_pos = args
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/tmp")
            .unwrap();
        assert!(tmp_pos > ro_bind_pos, "--tmpfs /tmp must come after --ro-bind / /");
    }

    /// Regression test: without masking /run, a sandboxed `gpg` reaches
    /// the host's real, already-running gpg-agent/keyboxd via
    /// /run/user/<uid>/gnupg/ — a socket path GnuPG resolves independently
    /// of $HOME — silently defeating the sandbox's keyring isolation
    /// (confirmed live: it surfaced the real user's own imported keys from
    /// unrelated past builds instead of an empty or build-scoped keyring).
    #[test]
    fn run_is_masked() {
        let policy = SandboxPolicy::new("/tmp/pkg", true);
        let args = policy.bwrap_args(&["makepkg".into()]);
        assert!(
            args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/run"),
            "/run must be masked so gpg/gpg-agent can't reach the host's real keyring"
        );
    }

    #[test]
    fn extra_env_included() {
        let mut policy = SandboxPolicy::new("/tmp/pkg", true);
        policy.extra_env.push(("MAKEFLAGS".into(), "-j4".into()));
        let args = policy.bwrap_args(&["makepkg".into()]);
        let pos = args.iter().position(|a| a == "MAKEFLAGS").unwrap();
        assert_eq!(args[pos + 1], "-j4");
    }
}
