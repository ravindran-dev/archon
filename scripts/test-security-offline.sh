#!/usr/bin/env bash
# Actual security module unit tests with cached, compatible dev dependencies.
# This temporary workspace does NOT replace cargo test --workspace --locked.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d /tmp/archon-security-tests.XXXXXX)
trap 'rm -rf "$work"' EXIT
python3 - "$root" "$work" <<'PY'
import json
from pathlib import Path
import sys
root, work = map(Path, sys.argv[1:])
(work / 'Cargo.toml').write_text('''[package]
name = "archon-security-regressions"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
anyhow = "1"
libc = "0.2"
archon-sandbox = { path = ''' + json.dumps(str(root / 'crates/archon-sandbox')) + ''' }
[dev-dependencies]
tempfile = "=3.24.0"
fastrand = "=2.3.0"
[lib]
path = "lib.rs"
''')
(work / 'lib.rs').write_text(''.join('#[path = ' + json.dumps(str(root / 'crates/archon/src' / (module + '.rs'))) + '] pub mod ' + module + ';\n' for module in ['archive', 'pgp']))
PY
CARGO_TARGET_DIR=/tmp/archon-security-regressions-target cargo test --offline --manifest-path "$work/Cargo.toml" "$@"
