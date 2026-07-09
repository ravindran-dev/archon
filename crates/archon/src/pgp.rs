//! Best-effort PGP key import for AUR builds.
//!
//! makepkg's own signature verification (`verify_file_signature` in
//! `/usr/share/makepkg/integrity/verify_signature.sh`) is a plain `gpg
//! --verify` against whatever's already in `$GNUPGHOME` — it does *not*
//! auto-import from a package's `keys/pgp/` directory (that directory is
//! only consulted when building a `--source` tarball, per
//! `/usr/bin/makepkg`'s `srclinks` step). Normally a user imports a
//! maintainer's key into their own `~/.gnupg` once, by hand, the first time
//! they hit "unknown public key". Inside archon's sandbox that shortcut
//! doesn't exist at all: `$HOME` is redirected to the package's own build
//! directory (see `SandboxPolicy::base_env`), so every sandboxed build
//! starts with a completely empty keyring — even for a key the user has
//! already trusted on the real system.
//!
//! This mirrors what yay/paru already do: before the networked Fetch stage
//! runs, read `validpgpkeys` out of the PKGBUILD (plain text parsing, never
//! sourced/executed — same rule as the rest of archon) and import each key
//! into `$pkg_dir/.gnupg`, preferring the maintainer-bundled copy under
//! `keys/pgp/<FINGERPRINT>.asc` when present (same trust boundary as the
//! PKGBUILD itself already cloned via git, no network needed) and falling
//! back to a keyserver otherwise. Both are best-effort: any failure here
//! just leaves makepkg's own "unknown public key" error to explain itself,
//! exactly as if this step didn't exist.

use std::path::Path;
use std::process::Command;

const KEYSERVER: &str = "keyserver.ubuntu.com";

/// Parses `validpgpkeys=(...)` out of a PKGBUILD via plain text scanning —
/// good enough for the overwhelmingly common case of a static literal
/// array, which is how every observed real-world PKGBUILD writes it. A
/// PKGBUILD that computes this array dynamically just won't be matched,
/// same as if this function didn't exist.
fn extract_valid_pgp_keys(pkgbuild: &str) -> Vec<String> {
    let Some(start) = pkgbuild.find("validpgpkeys=(") else {
        return Vec::new();
    };
    let after = &pkgbuild[start + "validpgpkeys=(".len()..];
    let Some(end) = after.find(')') else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let mut chars = after[..end].chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' || c == '"' {
            let quote = c;
            let key: String = chars.by_ref().take_while(|&c| c != quote).collect();
            if !key.is_empty() {
                keys.push(key);
            }
        }
    }
    keys
}

/// Imports every key `validpgpkeys` names into `$pkg_dir/.gnupg`, the
/// keyring the sandbox uses as `$GNUPGHOME` (`HOME` is set to `pkg_dir`
/// inside the sandbox). Never fails the build itself — worst case,
/// verification fails exactly as it would have without this step, and the
/// caller is still responsible for making sure `$pkg_dir/.gnupg` ends up
/// owned by whichever user the sandboxed build actually runs as.
pub fn import_build_keys(pkg_dir: &Path) {
    let Ok(pkgbuild) = std::fs::read_to_string(pkg_dir.join("PKGBUILD")) else {
        return;
    };
    let keys = extract_valid_pgp_keys(&pkgbuild);
    if keys.is_empty() {
        return;
    }
    let gnupg_home = pkg_dir.join(".gnupg");
    if std::fs::create_dir_all(&gnupg_home).is_err() {
        return;
    }
    // GnuPG refuses to treat a homedir as safe unless it's mode 0700 —
    // create_dir_all leaves it at the default (looser) mode, which
    // produced a harmless but noisy "unsafe ownership on homedir" warning
    // on every sandboxed build (confirmed live). Best-effort: a failure
    // here just means the warning comes back, not that verification
    // itself breaks.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&gnupg_home, std::fs::Permissions::from_mode(0o700));
    }
    let bundled_dir = pkg_dir.join("keys").join("pgp");
    for key in &keys {
        let bundled = bundled_dir.join(format!("{key}.asc"));
        // --no-permission-warning: on the monitored (--report) path this
        // import runs as root, before the privilege drop, against a
        // .gnupg that a prior unmonitored run may have already left
        // owned by the target user — root doesn't own it, so GnuPG warns
        // "unsafe ownership" even though the chown right after this
        // function returns (see run_monitored_build) fixes it before
        // anything privilege-dropped ever touches it. Cosmetic only; the
        // warning doesn't reflect an actual gap since nothing unprivileged
        // reads this directory before ownership is corrected.
        let status = if bundled.is_file() {
            Command::new("gpg")
                .args(["--homedir"])
                .arg(&gnupg_home)
                .args(["--batch", "--quiet", "--no-permission-warning", "--import"])
                .arg(&bundled)
                .status()
        } else {
            Command::new("gpg")
                .args(["--homedir"])
                .arg(&gnupg_home)
                .args([
                    "--batch",
                    "--quiet",
                    "--no-permission-warning",
                    "--keyserver",
                    KEYSERVER,
                    "--recv-keys",
                ])
                .arg(key)
                .status()
        };
        let _ = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_key() {
        let pkgbuild = "pkgname=spotify\nvalidpgpkeys=('E1096BCBFF6D418796DE78515384CE82BA52C83A') # comment\n";
        assert_eq!(
            extract_valid_pgp_keys(pkgbuild),
            vec!["E1096BCBFF6D418796DE78515384CE82BA52C83A".to_string()]
        );
    }

    #[test]
    fn extracts_multiple_multiline_keys() {
        let pkgbuild = "validpgpkeys=(\n  'AAAA1111'\n  \"BBBB2222\"\n)\n";
        assert_eq!(
            extract_valid_pgp_keys(pkgbuild),
            vec!["AAAA1111".to_string(), "BBBB2222".to_string()]
        );
    }

    #[test]
    fn missing_array_returns_empty() {
        assert!(extract_valid_pgp_keys("pkgname=foo\n").is_empty());
    }

    #[test]
    fn import_is_a_noop_without_a_pkgbuild() {
        let dir = tempfile::tempdir().unwrap();
        // Must not panic just because there's nothing to read.
        import_build_keys(dir.path());
        assert!(!dir.path().join(".gnupg").exists());
    }
}
