//! Policy engine: decides whether an observed syscall is expected build
//! activity or something to flag. Pure and I/O-free by design — DNS
//! resolution of a PKGBUILD's declared `source=()` hostnames happens in the
//! caller (needs network access, isn't unit-testable the same way), which
//! then hands this module a plain set of allowed IPv4 addresses.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use archon_monitor_common::{EventKind, SyscallEvent};

/// Build tools every PKGBUILD reasonably invokes: compilers/linkers,
/// archive tools, build-system drivers, and common coreutils. Deliberately
/// permissive — false positives on ordinary build steps would make the
/// report noise nobody reads. Extend via [`Policy::allow_tool`] for a
/// package with unusual (but legitimate) build-time tooling.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    // compilers / linkers
    "gcc", "cc", "g++", "c++", "clang", "clang++", "ld", "ld.bfd", "ld.gold", "ld.lld", "as",
    "ar", "nm", "objcopy", "objdump", "strip", "ranlib", "rustc", "cargo", "go",
    // build-system drivers
    "make", "cmake", "ninja", "meson", "autoconf", "automake", "autoreconf", "libtool", "m4",
    "bison", "flex", "pkg-config", "pkgconf",
    // archives / compression
    "tar", "gzip", "gunzip", "bzip2", "bunzip2", "xz", "unxz", "zstd", "unzstd", "bsdtar",
    "install", "patch",
    // scripting / text
    "sh", "bash", "python", "python3", "perl", "sed", "gawk", "awk", "grep", "find", "xargs",
    // fs / coreutils commonly shelled out to by build scripts
    "cp", "mv", "rm", "mkdir", "ln", "touch", "cat", "echo", "printf", "test", "true", "false",
    "dirname", "basename", "readlink", "realpath", "id", "uname", "nproc", "env", "which",
    "date", "sort", "wc", "chmod", "chown", "getopt", "cut",
    // packaging / checksums (makepkg itself and its helpers)
    "makepkg", "bwrap", "fakeroot", "faked", "gpg", "sha256sum", "sha512sum", "b2sum", "md5sum",
    "gdb-add-index", "install-info", "gettext", "tput",
    // fetch tools makepkg itself uses to download declared sources
    "curl", "wget", "git",
];

/// Bare filenames archon's own sandbox setup (bwrap's unprivileged user
/// namespace configuration) writes as a normal, universal part of Linux
/// sandboxing — confirmed live: eBPF captures these as relative opens
/// (no directory prefix), since bwrap opens them relative to an
/// already-open `/proc/self` fd rather than by absolute path.
const NAMESPACE_SETUP_FILES: &[&str] = &["uid_map", "gid_map", "setgroups"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagReason {
    /// An exec'd binary is neither on the tool allow-list nor inside the
    /// build directory (so not something the package itself just built).
    UnexpectedExec { path: String },
    /// A write (or create/truncate) `open` targeted a path outside the
    /// build directory — the sandbox's mount policy should already block
    /// this at the kernel level; seeing it here means it was *attempted*.
    WriteOutsideBuildDir { path: String },
    /// A TCP/UDP connect to an address that doesn't match any declared
    /// `source=()` URL's resolved IP.
    UndeclaredConnection { addr: Ipv4Addr, port: u16 },
}

impl std::fmt::Display for FlagReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlagReason::UnexpectedExec { path } => {
                write!(f, "executed unexpected program: {path}")
            }
            FlagReason::WriteOutsideBuildDir { path } => {
                write!(f, "wrote outside \\$srcdir/\\$pkgdir: {path}")
            }
            FlagReason::UndeclaredConnection { addr, port } => {
                write!(f, "connected to undeclared address: {addr}:{port}")
            }
        }
    }
}

/// Policy for one build: what's in-bounds for this specific package.
pub struct Policy {
    build_dir: PathBuf,
    allowed_tools: HashSet<String>,
    allowed_ips: HashSet<Ipv4Addr>,
}

impl Policy {
    /// `build_dir` is the AUR package's clone directory — the same path
    /// passed to [`archon_sandbox::SandboxRunner::new`], under which
    /// `$srcdir`/`$pkgdir` live. `allowed_ips` should be the resolved IPv4
    /// addresses of every hostname in the PKGBUILD's `source=()` array.
    pub fn new(build_dir: impl Into<PathBuf>, allowed_ips: HashSet<Ipv4Addr>) -> Self {
        Self {
            build_dir: build_dir.into(),
            allowed_tools: DEFAULT_ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect(),
            allowed_ips,
        }
    }

    pub fn allow_tool(&mut self, name: impl Into<String>) -> &mut Self {
        self.allowed_tools.insert(name.into());
        self
    }

    /// `None` for events that pass; `Some(reason)` for events to flag.
    pub fn evaluate(&self, event: &SyscallEvent) -> Option<FlagReason> {
        match EventKind::from_u8(event.kind)? {
            EventKind::Exec => self.evaluate_exec(event),
            EventKind::Open => self.evaluate_open(event),
            EventKind::Connect => self.evaluate_connect(event),
        }
    }

    fn evaluate_exec(&self, event: &SyscallEvent) -> Option<FlagReason> {
        let path = event.path_str();
        if path.is_empty() {
            return None; // unresolvable path — nothing actionable to report
        }
        if self.is_inside_build_dir(path) {
            return None; // running something the package itself just built
        }
        let basename = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        if self.allowed_tools.contains(&basename) {
            return None;
        }
        Some(FlagReason::UnexpectedExec { path: path.to_string() })
    }

    fn evaluate_open(&self, event: &SyscallEvent) -> Option<FlagReason> {
        if !event.is_write_open() {
            return None; // reads (headers, libs, pacman db, ...) are expected
        }
        let path = event.path_str();
        if path.is_empty() || self.is_inside_build_dir(path) {
            return None;
        }
        // /dev/null and friends are a universally harmless write target
        // (compilers redirect there constantly) and not part of any
        // package's output — exempt rather than flag every -o /dev/null.
        if path.starts_with("/dev/") || path.starts_with("/proc/self/") || path == "/tmp" {
            return None;
        }
        // Bare (no directory prefix) opens of the sandbox's own
        // unprivileged-userns setup files — see NAMESPACE_SETUP_FILES.
        if NAMESPACE_SETUP_FILES.contains(&path) {
            return None;
        }
        Some(FlagReason::WriteOutsideBuildDir { path: path.to_string() })
    }

    fn evaluate_connect(&self, event: &SyscallEvent) -> Option<FlagReason> {
        if event.addr_v4 == 0 {
            return None; // failed to resolve the address at capture time
        }
        let addr = Ipv4Addr::from(event.addr_v4.to_be());
        if self.allowed_ips.contains(&addr) {
            return None;
        }
        Some(FlagReason::UndeclaredConnection { addr, port: event.port })
    }

    fn is_inside_build_dir(&self, path: &str) -> bool {
        Path::new(path).starts_with(&self.build_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archon_monitor_common::open_flags;

    fn exec_event(path: &str) -> SyscallEvent {
        let mut ev = SyscallEvent::zeroed(EventKind::Exec);
        ev.path[..path.len()].copy_from_slice(path.as_bytes());
        ev.path_len = path.len() as u16;
        ev
    }

    fn open_event(path: &str, flags: u32) -> SyscallEvent {
        let mut ev = SyscallEvent::zeroed(EventKind::Open);
        ev.path[..path.len()].copy_from_slice(path.as_bytes());
        ev.path_len = path.len() as u16;
        ev.open_flags = flags;
        ev
    }

    fn connect_event(addr: Ipv4Addr, port: u16) -> SyscallEvent {
        let mut ev = SyscallEvent::zeroed(EventKind::Connect);
        ev.addr_v4 = u32::from(addr).to_be();
        ev.port = port;
        ev
    }

    fn policy() -> Policy {
        let mut ips = HashSet::new();
        ips.insert(Ipv4Addr::new(140, 82, 121, 4)); // github.com, say
        Policy::new("/home/user/.cache/archon/build/foo", ips)
    }

    #[test]
    fn allow_listed_tool_passes() {
        assert_eq!(policy().evaluate(&exec_event("/usr/bin/gcc")), None);
        assert_eq!(policy().evaluate(&exec_event("/usr/bin/make")), None);
    }

    #[test]
    fn unexpected_exec_is_flagged() {
        let got = policy().evaluate(&exec_event("/tmp/payload"));
        assert_eq!(
            got,
            Some(FlagReason::UnexpectedExec { path: "/tmp/payload".into() })
        );
    }

    #[test]
    fn exec_inside_build_dir_passes() {
        let ev = exec_event("/home/user/.cache/archon/build/foo/src/foo-1.0/configure");
        assert_eq!(policy().evaluate(&ev), None);
    }

    #[test]
    fn readonly_open_outside_build_dir_passes() {
        let ev = open_event("/usr/include/stdio.h", 0);
        assert_eq!(policy().evaluate(&ev), None);
    }

    #[test]
    fn write_open_outside_build_dir_is_flagged() {
        let ev = open_event("/etc/passwd", open_flags::O_WRONLY);
        assert_eq!(
            policy().evaluate(&ev),
            Some(FlagReason::WriteOutsideBuildDir { path: "/etc/passwd".into() })
        );
    }

    #[test]
    fn write_open_inside_build_dir_passes() {
        let ev = open_event(
            "/home/user/.cache/archon/build/foo/pkg/foo/usr/bin/foo",
            open_flags::O_CREAT | open_flags::O_WRONLY,
        );
        assert_eq!(policy().evaluate(&ev), None);
    }

    #[test]
    fn dev_null_write_is_exempt() {
        let ev = open_event("/dev/null", open_flags::O_WRONLY);
        assert_eq!(policy().evaluate(&ev), None);
    }

    #[test]
    fn namespace_setup_files_are_exempt() {
        // bwrap's own unprivileged userns setup, confirmed live: eBPF
        // captures these as bare relative opens (no directory prefix).
        for name in ["uid_map", "gid_map", "setgroups"] {
            let ev = open_event(name, open_flags::O_WRONLY);
            assert_eq!(policy().evaluate(&ev), None, "{name} should be exempt");
        }
    }

    #[test]
    fn bwrap_and_makepkg_internals_are_allow_listed() {
        for tool in [
            "bwrap", "date", "gettext", "tput", "sort", "wc", "chmod", "chown", "getopt", "cut",
        ] {
            let ev = exec_event(&format!("/usr/bin/{tool}"));
            assert_eq!(policy().evaluate(&ev), None, "{tool} should be allow-listed");
        }
    }

    #[test]
    fn declared_connection_passes() {
        let ev = connect_event(Ipv4Addr::new(140, 82, 121, 4), 443);
        assert_eq!(policy().evaluate(&ev), None);
    }

    #[test]
    fn undeclared_connection_is_flagged() {
        let ev = connect_event(Ipv4Addr::new(6, 6, 6, 6), 4444);
        assert_eq!(
            policy().evaluate(&ev),
            Some(FlagReason::UndeclaredConnection {
                addr: Ipv4Addr::new(6, 6, 6, 6),
                port: 4444
            })
        );
    }

    #[test]
    fn custom_allowed_tool() {
        let mut p = policy();
        p.allow_tool("mycustomtool");
        assert_eq!(p.evaluate(&exec_event("/usr/bin/mycustomtool")), None);
    }
}
