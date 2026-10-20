#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked --offline --bin archon
work=$(mktemp -d /tmp/archon-bench.XXXXXX)
trap 'rm -rf "$work"' EXIT
python3 - "$work/probe" <<'PY'
import subprocess, sys
from pathlib import Path
command = ['rustc', '--edition=2024', '-O', 'scripts/bench-resolver.rs', '-L', 'dependency=target/debug/deps', '-o', sys.argv[1]]
for name in ['archon_repo', 'archon_resolver']:
    library = max(Path('target/debug/deps').glob('lib' + name + '-*.rlib'), key=lambda p: p.stat().st_mtime)
    command += ['--extern', name + '=' + str(library)]
subprocess.run(command, check=True)
PY
"$work/probe"
