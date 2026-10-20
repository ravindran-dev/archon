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
//! Key import is automatic, but runs only inside the existing bubblewrap
//! sandbox as the build user. The host never creates or chmods a keyring and
//! never starts GPG against package-controlled configuration. makepkg remains
//! responsible for signature verification; import failures are reported and
//! do not disable verification.

use archon_sandbox::{BuildStage, SandboxRunner};
use std::path::Path;

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

/// Prepare keys with the ordinary unprivileged sandbox runner.
pub fn import_build_keys(pkg_dir: &Path) {
    let runner = SandboxRunner::new(pkg_dir);
    import_build_keys_with_runner(pkg_dir, &runner);
}

/// Monitored callers supply a runner configured to drop to the build user.
/// Do not add host-side keyring filesystem operations here: even a preliminary
/// symlink check would race changes to the package-controlled directory.
pub fn import_build_keys_with_runner(pkg_dir: &Path, runner: &SandboxRunner) {
    let Ok(pkgbuild) = std::fs::read_to_string(pkg_dir.join("PKGBUILD")) else {
        return;
    };
    let keys = extract_valid_pgp_keys(&pkgbuild);
    if keys.is_empty() {
        return;
    }
    // Refuse an obvious escape early. The sandbox's read-only host mount is
    // still required to protect against replacement after this check.
    if pkg_dir.join(".gnupg").is_symlink() {
        eprintln!("warning: refusing PGP preparation: .gnupg is a symlink");
        return;
    }
    let mut command = vec![
        "/bin/sh".into(),
        "-c".into(),
        IMPORT_SCRIPT.into(),
        "archon-key-import".into(),
        KEYSERVER.into(),
    ];
    for key in keys {
        if !matches!(key.len(), 40 | 64) || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            eprintln!("warning: ignoring invalid validpgpkeys fingerprint {key:?}");
            continue;
        }
        command.push(key);
    }
    if command.len() == 5 {
        return;
    }
    match runner
        .spawn_command(BuildStage::Fetch, &command)
        .and_then(|mut child| child.wait().map_err(archon_sandbox::SandboxError::Spawn))
    {
        Ok(status) if status.success() => {}
        result => eprintln!(
            "warning: sandboxed PGP key import failed ({result:?}); makepkg signature verification is still required"
        ),
    }
}

// The script is constant; untrusted fingerprints are separate argv elements,
// restricted to full hexadecimal fingerprints. --no-options ignores gpg.conf.
// All filesystem and subprocess operations below occur inside bwrap, after
// the build-user privilege drop. An outside symlink therefore hits a read-only
// host mount (or a private /tmp, /home, /root or /run mount), never a writable host path.
const IMPORT_SCRIPT: &str = r#"
set -eu
keyserver=$1
shift
if [ -L "$HOME/.gnupg" ]; then
    echo 'refusing symlinked build keyring' >&2
    exit 1
fi
umask 077
mkdir -p -- "$HOME/.gnupg"
chmod 700 -- "$HOME/.gnupg"
for key do
    bundled="$HOME/keys/pgp/$key.asc"
    if [ -f "$bundled" ]; then
        gpg --no-options --homedir "$HOME/.gnupg" --batch --quiet --import -- "$bundled"
    else
        gpg --no-options --homedir "$HOME/.gnupg" --batch --quiet --keyserver "$keyserver" --recv-keys -- "$key"
    fi
done
"#;

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
