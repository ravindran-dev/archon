//! Extracts declared source hostnames from an AUR package's `.SRCINFO` so
//! the monitor's policy engine knows which network destinations a build is
//! allowed to reach.
//!
//! Deliberately reads `.SRCINFO`, never the `PKGBUILD` itself: `.SRCINFO`
//! is static, AUR-generated metadata (plain `key = value` text), while a
//! `PKGBUILD` is executable bash — sourcing it just to read `source=()`
//! would mean running an untrusted, unsandboxed build script before the
//! sandbox even exists. Every AUR package is required to ship a
//! `.SRCINFO` alongside its `PKGBUILD`, so this loses no real coverage.

use std::collections::HashSet;
use std::net::{Ipv4Addr, ToSocketAddrs};
use std::path::Path;

/// Parses `pkg_dir/.SRCINFO` and resolves every declared `source=()`
/// URL's hostname to its IPv4 addresses. Best-effort: a missing file,
/// unparseable line, or DNS failure for one host is skipped rather than
/// failing the whole call — the policy engine treats an empty/partial set
/// conservatively (nothing not in it is allowed), which is the safe
/// direction to fail in.
pub fn resolve_allowed_ips(pkg_dir: &Path) -> HashSet<Ipv4Addr> {
    let mut ips = HashSet::new();
    let Ok(contents) = std::fs::read_to_string(pkg_dir.join(".SRCINFO")) else {
        return ips;
    };
    for host in source_hosts(&contents) {
        if let Ok(addrs) = (host.as_str(), 443u16).to_socket_addrs() {
            for addr in addrs {
                if let std::net::IpAddr::V4(v4) = addr.ip() {
                    ips.insert(v4);
                }
            }
        }
    }
    ips
}

/// Extracts hostnames from every `source` / `source_<arch>` line's URL.
/// Local files (no `::`-prefixed URL, or no recognized scheme) are
/// skipped — they're read from the clone directory, not fetched.
fn source_hosts(srcinfo: &str) -> HashSet<String> {
    let mut hosts = HashSet::new();
    for line in srcinfo.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key != "source" && !key.starts_with("source_") {
            continue;
        }
        let value = value.trim();
        // `filename::url` form strips the local filename prefix; a bare
        // `source=url` has no `::`.
        let url = value.split_once("::").map(|(_, u)| u).unwrap_or(value);
        if let Some(host) = extract_host(url) {
            hosts.insert(host);
        }
    }
    hosts
}

fn extract_host(url: &str) -> Option<String> {
    // makepkg prefixes VCS source transports, e.g. git+https://. The
    // underlying HTTP/HTTPS/FTP hostname is still a declared destination.
    let url = match url.split_once('+') {
        Some(("git" | "hg" | "svn" | "bzr", transport)) => transport,
        _ => url,
    };
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ftp://"))?;
    let host = rest.split(['/', ':', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
pkgbase = tty-clock
\tpkgver = 2.3
\turl = https://github.com/xorg62/tty-clock
\tsource = tty-clock-2.3.tar.gz::https://github.com/xorg62/tty-clock/archive/v2.3.tar.gz
\tsource = 0001-include-format-specifier-in-printf.patch
\tsource_x86_64 = https://cdn.example.org/extra.bin
\tsource = ftp://ftp.gnu.org/gnu/foo.tar.gz

pkgname = tty-clock
";

    #[test]
    fn vcs_https_sources_declare_their_network_host() {
        let hosts = source_hosts(
            "source = scanner::git+https://git.example.org/scanner.git#branch=main\n\
             source_x86_64 = hg+https://hg.example.org/scanner\n",
        );
        assert!(
            hosts.contains("git.example.org"),
            "missing declared Git source host: {hosts:?}"
        );
        assert!(hosts.contains("hg.example.org"));
    }

    #[test]
    fn extracts_hosts_from_source_lines() {
        let hosts = source_hosts(SAMPLE);
        assert!(hosts.contains("github.com"));
        assert!(hosts.contains("cdn.example.org"));
        assert!(hosts.contains("ftp.gnu.org"));
        assert_eq!(
            hosts.len(),
            3,
            "local (no-scheme) source must not produce a host: {hosts:?}"
        );
    }

    #[test]
    fn ignores_local_sources_without_scheme() {
        let hosts = source_hosts("\tsource = local-patch.diff\n");
        assert!(hosts.is_empty());
    }

    #[test]
    fn strips_port_and_path_and_query() {
        assert_eq!(
            extract_host("https://example.com:8443/path?query=1#frag"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn missing_srcinfo_yields_empty_set() {
        let dir = std::env::temp_dir().join("archon-srcinfo-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resolve_allowed_ips(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
