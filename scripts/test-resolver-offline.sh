#!/usr/bin/env bash
# Test actual source through a minimal workspace when unrelated reqwest
# dependencies prevent Cargo from resolving the full workspace offline.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d /tmp/archon-resolver-check.XXXXXX)
trap 'rm -rf "$work"' EXIT
for crate in archon-types archon-resolver; do
    mkdir -p "$work/crates/$crate"
    cp "$root/crates/$crate/Cargo.toml" "$work/crates/$crate/Cargo.toml"
    ln -s "$root/crates/$crate/src" "$work/crates/$crate/src"
done
cat > "$work/Cargo.toml" <<'TOML'
[workspace]
resolver = "2"
members = ["crates/archon-types", "crates/archon-resolver"]
[workspace.package]
version = "0.2.1"
edition = "2024"
license = "MIT"
repository = "https://github.com/ravindran-dev/archon"
[workspace.dependencies]
archon-types = { path = "crates/archon-types" }
serde = { version = "1", features = ["derive"] }
thiserror = "2"
TOML
# Keep build artifacts outside the temporary manifests so reruns are cheap.
export CARGO_TARGET_DIR="${ARCHON_AUDIT_TARGET_DIR:-/tmp/archon-audit-resolver-target}"
if [[ "${1:-}" == --clippy ]]; then
    shift
    cargo clippy --manifest-path "$work/Cargo.toml" --offline "$@"
else
    cargo test --manifest-path "$work/Cargo.toml" --offline "$@"
fi
