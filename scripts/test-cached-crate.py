#!/usr/bin/env python3
"""Compile actual crate unit tests against existing rlibs when Cargo fetch is blocked.

No sources or tests are rewritten; requested test filters are passed to libtest.
Run cargo build --locked --offline first to refresh internal dependencies.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

root = Path(__file__).resolve().parents[1]
crate, *args = sys.argv[1:]
manifest = tomllib.loads((root / 'crates' / crate / 'Cargo.toml').read_text())
workspace = tomllib.loads((root / 'Cargo.toml').read_text())
version = manifest['package'].get('version')
if not isinstance(version, str):
    version = workspace['workspace']['package']['version']
deps = root / 'target/debug/deps'
with tempfile.TemporaryDirectory(prefix='archon-cached-tests-') as directory:
    binary = Path(directory) / 'tests'
    command = ['rustc', '--edition=2024', '--test', str(root / 'crates' / crate / 'src/lib.rs'),
               '-L', f'dependency={deps}', '-o', str(binary)]
    for dependency in dict(manifest.get('dependencies', {}), **manifest.get('dev-dependencies', {})):
        name = dependency.replace('-', '_')
        libraries = sorted(deps.glob(f'lib{name}-*.rlib'), key=lambda p: p.stat().st_mtime)
        if not libraries:
            raise SystemExit(f'UNVERIFIED: missing compiled dependency {name}; cannot compile {crate} tests')
        command += ['--extern', f'{name}={libraries[-1]}']
    env = os.environ.copy()
    env.update(CARGO_PKG_VERSION=version, CARGO_MANIFEST_DIR=str(root / 'crates' / crate))
    subprocess.run(command, env=env, check=True)
    raise SystemExit(subprocess.run([str(binary), *args], env=env).returncode)
